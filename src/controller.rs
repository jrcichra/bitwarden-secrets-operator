//! The `BitwardenSecret` CRD and its reconciler.

use crate::metrics;
use crate::sm::{Item, SecretsManager};
use futures::StreamExt;
use k8s_openapi::api::core::v1::Secret;
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
const DEFAULT_NOTE_KEY: &str = "notes";
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Materializes the Secrets Manager secret `spec.name` as a `Secret` with the
/// same name and namespace as this object.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(group = "jrcichra.dev", version = "v1", kind = "BitwardenSecret")]
#[kube(shortname = "bws", namespaced)]
pub struct BitwardenSecretSpec {
    /// Secrets Manager secret name.
    name: String,
    /// `Secret` data key for a non-login value. Defaults to `notes`.
    key: Option<String>,
    /// `Secret` type, e.g. `kubernetes.io/dockerconfigjson`.
    #[serde(rename = "type")]
    type_: Option<String>,
}

type Cache = Arc<Mutex<HashMap<String, Item>>>;

struct Context {
    client: Client,
    cache: Cache,
    requeue_after: Duration,
    failures: Mutex<HashMap<ObjectRef<BitwardenSecret>, u32>>,
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

/// 10s, 20s, 40s, ... capped at `MAX_BACKOFF`.
fn backoff(failures: u32) -> Duration {
    Duration::from_secs(5)
        .saturating_mul(2u32.saturating_pow(failures))
        .min(MAX_BACKOFF)
}

/// `Secret` data for an item.
fn secret_data(item: &Item, key: Option<&str>) -> BTreeMap<String, String> {
    match item {
        Item::Login { username, password } => BTreeMap::from([
            ("username".to_string(), username.clone()),
            ("password".to_string(), password.clone()),
        ]),
        Item::Note(value) => {
            BTreeMap::from([(key.unwrap_or(DEFAULT_NOTE_KEY).to_string(), value.clone())])
        }
    }
}

async fn reconcile(obj: Arc<BitwardenSecret>, ctx: Arc<Context>) -> Result<Action, Error> {
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

    let secret = json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "ownerReferences": [owner],
            "annotations": { "lastReconciled": chrono::Utc::now().to_rfc3339() },
        },
        "type": obj.spec.type_,
        "stringData": secret_data(&item, obj.spec.key.as_deref()),
    });

    Api::<Secret>::namespaced(ctx.client.clone(), &namespace)
        .patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&secret),
        )
        .await
        .map_err(Error::Apply)?;

    ctx.failures
        .lock()
        .expect("failures lock poisoned")
        .remove(&ObjectRef::from_obj(&*obj));
    metrics::LAST_SUCCESSFUL_RECONCILE.set(chrono::Utc::now().timestamp());
    metrics::RECONCILE_SUCCESS_TOTAL.inc();

    // requeue so Secrets Manager edits reach the Secret
    Ok(Action::requeue(ctx.requeue_after))
}

fn error_policy(obj: Arc<BitwardenSecret>, error: &Error, ctx: Arc<Context>) -> Action {
    let failures = {
        let mut map = ctx.failures.lock().expect("failures lock poisoned");
        let n = map.entry(ObjectRef::from_obj(&*obj)).or_default();
        *n += 1;
        *n
    };
    warn!(
        object = %ObjectRef::from_obj(&*obj),
        failures,
        "reconcile failed: {error}"
    );
    metrics::RECONCILE_FAILURE_TOTAL.inc();
    Action::requeue(backoff(failures))
}

async fn refresh(sm: &SecretsManager, cache: &Cache) {
    match sm.fetch().await {
        Ok(items) => {
            info!("loaded {} secrets from Secrets Manager", items.len());
            metrics::CACHED_SECRETS.set(items.len().try_into().unwrap_or(i64::MAX));
            metrics::CACHE_REFRESH_SUCCESS_TOTAL.inc();
            *cache.lock().expect("cache lock poisoned") = items;
        }
        Err(e) => {
            // keep serving the previous cache
            warn!("failed to refresh secrets: {e:#}");
            metrics::CACHE_REFRESH_FAILURE_TOTAL.inc();
        }
    }
}

/// Run the controller until `shutdown` resolves.
pub async fn run(
    client: Client,
    sm: SecretsManager,
    interval: Duration,
    shutdown: impl Future<Output = ()> + Send + Sync + 'static,
) {
    let cache = Cache::default();
    refresh(&sm, &cache).await;

    let refresher = tokio::spawn({
        let cache = cache.clone();
        async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // first tick is immediate; we just refreshed
            loop {
                ticker.tick().await;
                refresh(&sm, &cache).await;
            }
        }
    });

    let ctx = Arc::new(Context {
        client: client.clone(),
        cache,
        requeue_after: interval,
        failures: Mutex::default(),
    });

    Controller::new(
        Api::<BitwardenSecret>::all(client),
        watcher::Config::default(),
    )
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
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff(1), Duration::from_secs(10));
        assert_eq!(backoff(2), Duration::from_secs(20));
        assert_eq!(backoff(10), MAX_BACKOFF);
        assert_eq!(backoff(u32::MAX), MAX_BACKOFF);
    }

    #[test]
    fn login_ignores_key_and_note_uses_it() {
        let login = Item::Login {
            username: "u".into(),
            password: "p".into(),
        };
        assert_eq!(
            secret_data(&login, Some("ignored"))
                .keys()
                .collect::<Vec<_>>(),
            ["password", "username"]
        );
        let note = Item::Note("v".into());
        assert_eq!(secret_data(&note, None)["notes"], "v");
        assert_eq!(
            secret_data(&note, Some(".dockerconfigjson"))[".dockerconfigjson"],
            "v"
        );
    }
}
