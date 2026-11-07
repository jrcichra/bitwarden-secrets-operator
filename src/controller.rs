//! The `BitwardenSecret` CRD and its reconciler.

use crate::metrics;
use crate::sm::{Item, SecretsManager};
use futures::StreamExt;
use k8s_openapi::ByteString;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, ObjectMeta, Time};
use kube::{
    Client, CustomResource, Resource, ResourceExt,
    api::{Api, Patch, PatchParams},
    runtime::{
        controller::{Action, Controller},
        reflector::ObjectRef,
        watcher,
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use tracing::{info, warn};

const FIELD_MANAGER: &str = "bitwarden-secrets-operator.jrcichra.dev";
const MANAGED_BY: (&str, &str) = ("app.kubernetes.io/managed-by", "bitwarden-secrets-operator");
const DEFAULT_NOTE_KEY: &str = "notes";
const READY: &str = "Ready";
/// Retry delay after a failed reconcile or startup fetch.
const RETRY: Duration = Duration::from_secs(60);
/// Safety-net resync. Real changes arrive via the watches and via
/// `reconcile_all_on` when a Secrets Manager refresh sees new content.
const RESYNC: Duration = Duration::from_secs(3600);

/// Materializes the Secrets Manager secret `spec.name` as a `Secret` with the
/// same name and namespace as this object.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(group = "jrcichra.dev", version = "v1", kind = "BitwardenSecret")]
#[kube(shortname = "bws", namespaced, status = "BitwardenSecretStatus")]
#[kube(
    printcolumn = r#"{"name":"Secret","type":"string","jsonPath":".spec.name"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type==\"Ready\")].status"}"#,
    printcolumn = r#"{"name":"Reason","type":"string","jsonPath":".status.conditions[?(@.type==\"Ready\")].reason"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
pub struct BitwardenSecretSpec {
    /// Secrets Manager secret name.
    name: String,
    /// `Secret` data key for a non-login value. Defaults to `notes`.
    key: Option<String>,
    /// `Secret` type, e.g. `kubernetes.io/dockerconfigjson`.
    #[serde(rename = "type")]
    type_: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
pub struct BitwardenSecretStatus {
    /// Standard conditions; `Ready` says whether the `Secret` is in sync.
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

type Cache = Arc<Mutex<HashMap<String, Item>>>;

struct Context {
    client: Client,
    cache: Cache,
}

#[derive(Debug, Error)]
enum Error {
    #[error("secret '{0}' not found in Secrets Manager")]
    NotFound(String),
    #[error("object has no {0}")]
    MissingField(&'static str),
    #[error("failed to apply Secret: {0}")]
    Apply(#[source] kube::Error),
}

impl Error {
    /// `reason` for the `Ready` condition.
    fn reason(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "NotFound",
            Self::MissingField(_) => "InvalidObject",
            Self::Apply(_) => "ApplyFailed",
        }
    }
}

/// `Secret` data for an item.
fn secret_data(item: &Item, key: Option<&str>) -> BTreeMap<String, ByteString> {
    let entry = |k: &str, v: &str| (k.to_string(), ByteString(v.as_bytes().to_vec()));
    match item {
        Item::Login { username, password } => {
            BTreeMap::from([entry("username", username), entry("password", password)])
        }
        Item::Note(value) => BTreeMap::from([entry(key.unwrap_or(DEFAULT_NOTE_KEY), value)]),
    }
}

/// The `Ready` condition to record, or `None` when the current one already
/// says the same thing. Writing only on change keeps a status update from
/// re-triggering reconciles forever.
fn next_ready(
    current: Option<&Condition>,
    generation: Option<i64>,
    outcome: Result<(), &Error>,
    now: Time,
) -> Option<Condition> {
    let (status, reason, message) = match outcome {
        Ok(()) => ("True", "Synced", String::new()),
        Err(e) => ("False", e.reason(), e.to_string()),
    };
    let unchanged = current.is_some_and(|c| {
        c.status == status
            && c.reason == reason
            && c.message == message
            && c.observed_generation == generation
    });
    if unchanged {
        return None;
    }
    let last_transition_time = match current {
        Some(c) if c.status == status => c.last_transition_time.clone(),
        _ => now,
    };
    Some(Condition {
        type_: READY.into(),
        status: status.into(),
        reason: reason.into(),
        message,
        last_transition_time,
        observed_generation: generation,
    })
}

/// Server-side apply the `Secret`. Content is deterministic, so re-applying an
/// unchanged secret is a no-op and doesn't wake the owned-`Secret` watch.
async fn apply_secret(obj: &BitwardenSecret, ctx: &Context) -> Result<(), Error> {
    let item = ctx
        .cache
        .lock()
        .expect("cache lock poisoned")
        .get(&obj.spec.name)
        .cloned()
        .ok_or_else(|| Error::NotFound(obj.spec.name.clone()))?;

    let name = obj
        .metadata
        .name
        .clone()
        .ok_or(Error::MissingField("name"))?;
    let namespace = obj.namespace().ok_or(Error::MissingField("namespace"))?;
    let owner = obj
        .controller_owner_ref(&())
        .ok_or(Error::MissingField("uid"))?;

    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(namespace.clone()),
            owner_references: Some(vec![owner]),
            labels: Some(BTreeMap::from([(
                MANAGED_BY.0.to_string(),
                MANAGED_BY.1.to_string(),
            )])),
            ..Default::default()
        },
        type_: obj.spec.type_.clone(),
        data: Some(secret_data(&item, obj.spec.key.as_deref())),
        ..Default::default()
    };

    let api = Api::<Secret>::namespaced(ctx.client.clone(), &namespace);
    let params = PatchParams::apply(FIELD_MANAGER).force();
    let patch = Patch::Apply(&secret);
    let apply = || api.patch(&name, &params, &patch);
    match apply().await {
        Ok(_) => Ok(()),
        // `type` is immutable, so a changed spec.type needs a fresh Secret
        Err(kube::Error::Api(status))
            if status.code == 422 && status.message.contains("immutable") =>
        {
            info!(secret = %name, namespace = %namespace, "recreating Secret: {}", status.message);
            api.delete(&name, &Default::default())
                .await
                .map_err(Error::Apply)?;
            apply().await.map(|_| ()).map_err(Error::Apply)
        }
        Err(e) => Err(Error::Apply(e)),
    }
}

async fn write_ready(obj: &BitwardenSecret, client: &Client, outcome: Result<(), &Error>) {
    let current = obj
        .status
        .as_ref()
        .and_then(|s| s.conditions.iter().find(|c| c.type_ == READY));
    let now = Time(k8s_openapi::jiff::Timestamp::now());
    let Some(ready) = next_ready(current, obj.metadata.generation, outcome, now) else {
        return;
    };
    let (Some(name), Some(namespace)) = (obj.metadata.name.as_deref(), obj.namespace()) else {
        return;
    };
    let status = json!({
        "apiVersion": BitwardenSecret::api_version(&()),
        "kind": BitwardenSecret::kind(&()),
        "status": { "conditions": [ready] },
    });
    if let Err(e) = Api::<BitwardenSecret>::namespaced(client.clone(), &namespace)
        .patch_status(
            name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&status),
        )
        .await
    {
        // status is informational; don't fail the reconcile over it
        warn!(object = %ObjectRef::from_obj(obj), "failed to update status: {e}");
    }
}

async fn reconcile(obj: Arc<BitwardenSecret>, ctx: Arc<Context>) -> Result<Action, Error> {
    let result = apply_secret(&obj, &ctx).await;
    write_ready(&obj, &ctx.client, result.as_ref().map(|_| ())).await;
    result?;
    metrics::LAST_SUCCESSFUL_RECONCILE.set(k8s_openapi::jiff::Timestamp::now().as_second());

    Ok(Action::requeue(RESYNC))
}

fn error_policy(obj: Arc<BitwardenSecret>, error: &Error, _: Arc<Context>) -> Action {
    warn!(object = %ObjectRef::from_obj(&*obj), "reconcile failed: {error}");
    metrics::RECONCILE_FAILURE_TOTAL.inc();
    Action::requeue(RETRY)
}

/// Reload the cache. `Ok(true)` when the contents changed.
async fn refresh(sm: &SecretsManager, cache: &Cache) -> anyhow::Result<bool> {
    let items = sm.fetch().await.inspect_err(|e| {
        warn!("failed to refresh secrets: {e:#}");
        metrics::CACHE_REFRESH_FAILURE_TOTAL.inc();
    })?;
    metrics::CACHED_SECRETS.set(items.len().try_into().unwrap_or(i64::MAX));
    let mut cache = cache.lock().expect("cache lock poisoned");
    let changed = *cache != items;
    if changed {
        info!(
            "loaded {} secrets from Secrets Manager (changed)",
            items.len()
        );
        *cache = items;
    }
    Ok(changed)
}

/// Run the controller until `shutdown` resolves.
pub async fn run(
    client: Client,
    sm: SecretsManager,
    interval: Duration,
    shutdown: impl Future<Output = ()> + Send + Sync + 'static,
) {
    let cache = Cache::default();
    // an empty cache would mark every object NotFound, so wait for a first load
    while refresh(&sm, &cache).await.is_err() {
        tokio::time::sleep(RETRY).await;
    }

    // a refresh that changes anything reconciles every object right away
    let (changed_tx, changed_rx) = futures::channel::mpsc::unbounded::<()>();
    let refresher = tokio::spawn({
        let cache = cache.clone();
        async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // first tick is immediate; we just loaded
            loop {
                ticker.tick().await;
                // errors are logged in refresh; keep serving the old cache
                if let Ok(true) = refresh(&sm, &cache).await {
                    let _ = changed_tx.unbounded_send(());
                }
            }
        }
    });

    let ctx = Arc::new(Context {
        client: client.clone(),
        cache,
    });

    // watch only Secrets we made, so an edited or deleted one is put back at
    // once (and the cache stays small)
    let owned = watcher::Config::default().labels(&format!("{}={}", MANAGED_BY.0, MANAGED_BY.1));

    Controller::new(
        Api::<BitwardenSecret>::all(client.clone()),
        watcher::Config::default(),
    )
    .owns(Api::<Secret>::all(client), owned)
    .reconcile_all_on(changed_rx)
    .graceful_shutdown_on(shutdown)
    .run(reconcile, error_policy, ctx)
    .for_each(|res| async move {
        if let Ok((obj, _)) = res {
            info!(object = %obj, "reconciled");
        }
        // errors are already logged by error_policy
    })
    .await;

    refresher.abort();
    info!("controller stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_ignores_key_and_note_uses_it() {
        let login = Item::Login {
            username: "u".into(),
            password: "p".into(),
        };
        let data = secret_data(&login, Some("ignored"));
        assert_eq!(data.keys().collect::<Vec<_>>(), ["password", "username"]);
        assert_eq!(data["username"].0, b"u");

        let note = Item::Note("v".into());
        assert_eq!(secret_data(&note, None)["notes"].0, b"v");
        assert_eq!(
            secret_data(&note, Some(".dockerconfigjson"))[".dockerconfigjson"].0,
            b"v"
        );
    }

    fn t(secs: i64) -> Time {
        Time(k8s_openapi::jiff::Timestamp::from_second(secs).unwrap())
    }

    #[test]
    fn ready_condition_only_changes_on_change() {
        let err = Error::NotFound("x".into());

        let first = next_ready(None, Some(1), Ok(()), t(1)).expect("first write");
        assert_eq!(
            (first.status.as_str(), first.reason.as_str()),
            ("True", "Synced")
        );
        assert_eq!(first.last_transition_time, t(1));

        // same outcome again: no write (this is what prevents a hot loop)
        assert_eq!(next_ready(Some(&first), Some(1), Ok(()), t(2)), None);

        // new generation, still ready: write, but keep the transition time
        let regen = next_ready(Some(&first), Some(2), Ok(()), t(3)).expect("generation bump");
        assert_eq!(regen.last_transition_time, t(1));

        // ready -> not ready: write with a new transition time
        let failed = next_ready(Some(&regen), Some(2), Err(&err), t(4)).expect("transition");
        assert_eq!(
            (failed.status.as_str(), failed.reason.as_str()),
            ("False", "NotFound")
        );
        assert_eq!(failed.last_transition_time, t(4));
        assert_eq!(next_ready(Some(&failed), Some(2), Err(&err), t(5)), None);
    }
}
