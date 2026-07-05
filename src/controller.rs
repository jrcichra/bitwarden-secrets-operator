use super::prometheus;
use crate::bw::BitwardenClient;
use crate::Args;
use chrono;
use futures::StreamExt;
use k8s_openapi::api::core::v1::Secret;
use kube::runtime::watcher;
use kube::Resource;
use kube::{
    api::{Api, Patch, PatchParams},
    runtime::controller::{Action, Controller},
    Client, CustomResource,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tracing::{info, warn};

#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(group = "jrcichra.dev", version = "v1", kind = "BitwardenSecret")]
#[kube(shortname = "bws", namespaced)]
pub struct BitwardenSecretSpec {
    name: String,
    key: Option<String>,
    #[serde(rename = "type")]
    type_: Option<String>,
}

/// Identifies a Kubernetes object for the purposes of tracking per-object retry state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ObjectKey {
    namespace: String,
    name: String,
}

impl ObjectKey {
    fn from_object(obj: &BitwardenSecret) -> Self {
        Self {
            namespace: obj
                .metadata
                .namespace
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            name: obj
                .metadata
                .name
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        }
    }
}

// Data we want access to in error/reconcile calls
struct Data {
    client: Client,
    cache: Arc<Mutex<HashMap<String, Value>>>,
    reconcile_interval: Duration,
    retry_counts: Arc<Mutex<HashMap<ObjectKey, u32>>>,
}

#[derive(Debug, Error)]
enum ReconcileError {
    #[error("Failed to create Secret: {0}")]
    SecretCreationFailed(#[source] kube::Error),
    #[error("MissingObjectKey: {0}")]
    MissingObjectKey(&'static str),
    #[error("BitwardenError: {0}")]
    BitwardenError(String),
    #[error("InvalidSecretData: {0}")]
    InvalidSecretData(String),
}

/// Calculate exponential backoff duration
fn calculate_backoff(attempt: u32) -> Duration {
    let base_secs = 5u64;
    let max_secs = 300u64; // 5 minutes max
    let backoff = base_secs.saturating_mul(2u64.saturating_pow(attempt));
    Duration::from_secs(backoff.min(max_secs))
}

/// Controller triggers this whenever our main object or our children changed
async fn reconcile(
    generator: Arc<BitwardenSecret>,
    ctx: Arc<Data>,
) -> Result<Action, ReconcileError> {
    let client = &ctx.client;
    let name = &generator.spec.name;
    let key = &generator.spec.key;
    let type_ = &generator.spec.type_;
    let mut contents = BTreeMap::new();

    // Get the secret from the cache
    let secret_value = {
        let cache_guard = ctx.cache.lock().unwrap();
        match cache_guard.get(name) {
            Some(value) => value.clone(),
            None => {
                return Err(ReconcileError::BitwardenError(format!(
                    "secret '{}' not found in cache",
                    name
                )));
            }
        }
    };

    // build the content for the secret here
    match secret_value.get("login") {
        Some(login) => {
            // set the username and password keys
            let username = login
                .get("username")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ReconcileError::InvalidSecretData(format!(
                        "login item '{}' is missing a string username",
                        name
                    ))
                })?;
            let password = login
                .get("password")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ReconcileError::InvalidSecretData(format!(
                        "login item '{}' is missing a string password",
                        name
                    ))
                })?;
            contents.insert("username".to_string(), username.to_string());
            contents.insert("password".to_string(), password.to_string());
        }
        None => {
            let notes_constant = "notes";
            let mut use_key = notes_constant;
            if let Some(key) = key {
                use_key = key.as_str();
            }
            match secret_value.get(notes_constant) {
                Some(notes) => {
                    let notes_str = notes.as_str().ok_or_else(|| {
                        ReconcileError::InvalidSecretData(format!(
                            "item '{}' has a non-string notes field",
                            name
                        ))
                    })?;
                    contents.insert(use_key.to_string(), notes_str.to_string());
                }
                None => {
                    return Err(ReconcileError::BitwardenError(format!(
                        "card/login not found for {}",
                        name
                    )));
                }
            }
        }
    }

    let oref = generator.controller_owner_ref(&()).unwrap();
    let current_time = chrono::offset::Utc::now();
    let mut annotations = BTreeMap::new();
    annotations.insert("lastReconciled".to_string(), current_time.to_rfc3339());

    let secret_name = generator
        .metadata
        .name
        .as_ref()
        .ok_or(ReconcileError::MissingObjectKey(".metadata.name"))?
        .clone();
    let namespace = generator
        .metadata
        .namespace
        .as_ref()
        .ok_or(ReconcileError::MissingObjectKey(".metadata.namespace"))?
        .clone();

    let secret_api = Api::<Secret>::namespaced(client.clone(), &namespace);

    let patch = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": secret_name,
            "namespace": namespace,
            "ownerReferences": [oref],
            "annotations": annotations,
        },
        "type": type_,
        "stringData": contents,
    });

    secret_api
        .patch(
            &secret_name,
            &PatchParams::apply("bitwarden-secrets-operator.jrcichra.dev").force(),
            &Patch::Apply(&patch),
        )
        .await
        .map_err(ReconcileError::SecretCreationFailed)?;

    // this object succeeded, so reset any accumulated backoff for it
    {
        let mut retry_guard = ctx.retry_counts.lock().unwrap();
        retry_guard.remove(&ObjectKey::from_object(&generator));
    }

    // let prometheus know we started the reconcile loop
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    prometheus::LAST_SUCCESSFUL_RECONCILE.set(now.as_secs().try_into().unwrap());
    prometheus::RECONCILE_SUCCESS_TOTAL.inc();

    // Requeue periodically to refresh secret from cache
    Ok(Action::requeue(ctx.reconcile_interval))
}

/// The controller triggers this on reconcile errors
fn error_policy(object: Arc<BitwardenSecret>, error: &ReconcileError, ctx: Arc<Data>) -> Action {
    let name = object
        .metadata
        .name
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let key = ObjectKey::from_object(&object);

    let attempt = {
        let mut retry_guard = ctx.retry_counts.lock().unwrap();
        let entry = retry_guard.entry(key).or_insert(0);
        *entry += 1;
        *entry
    };

    warn!(
        "reconcile failed for {} (attempt {}): {:?}",
        name, attempt, error
    );

    prometheus::RECONCILE_FAILURE_TOTAL.inc();

    // Use exponential backoff, escalating per-object, with max of 5 minutes
    Action::requeue(calculate_backoff(attempt))
}

pub async fn run(
    client: Client,
    args: Args,
    session: String,
    cache: Arc<Mutex<HashMap<String, Value>>>,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) -> anyhow::Result<()> {
    let bitwarden_secrets = Api::<BitwardenSecret>::all(client.clone());
    let bw = Arc::new(BitwardenClient::new(session));

    // Initial cache load
    info!("loading secrets into cache...");
    match bw.get_secrets(&args.folder).await {
        Ok(new_secrets) => {
            let mut cache_guard = cache.lock().unwrap();
            info!("loaded {} secrets into cache", new_secrets.len());
            prometheus::CACHED_SECRETS.set(new_secrets.len() as i64);
            prometheus::CACHE_REFRESH_SUCCESS_TOTAL.inc();
            *cache_guard = new_secrets;
        }
        Err(e) => {
            warn!("failed to load initial secrets: {}", e);
            prometheus::CACHE_REFRESH_FAILURE_TOTAL.inc();
        }
    }

    // Spawn background task to refresh cache periodically
    let bw_clone = bw.clone();
    let folder_clone = args.folder.clone();
    let cache_clone = cache.clone();
    let reconcile_interval = Duration::from_secs(args.reconcile_interval);

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(reconcile_interval).await;

            match bw_clone.get_secrets(&folder_clone).await {
                Ok(new_secrets) => {
                    let mut cache_guard = cache_clone.lock().unwrap();
                    info!("refreshed cache with {} secrets", new_secrets.len());
                    prometheus::CACHED_SECRETS.set(new_secrets.len() as i64);
                    prometheus::CACHE_REFRESH_SUCCESS_TOTAL.inc();
                    *cache_guard = new_secrets;
                }
                Err(e) => {
                    warn!("failed to refresh secrets cache (even after re-login): {}", e);
                    prometheus::CACHE_REFRESH_FAILURE_TOTAL.inc();
                }
            }
        }
    });

    // Create a shutdown signal from the receiver
    let _shutdown_signal = async move {
        let _ = shutdown_rx.await;
        info!("received shutdown signal, stopping controller...");
    };

    Controller::new(bitwarden_secrets, watcher::Config::default())
        .shutdown_on_signal()
        .run(
            reconcile,
            error_policy,
            Arc::new(Data {
                client,
                cache,
                reconcile_interval,
                retry_counts: Arc::new(Mutex::new(HashMap::new())),
            }),
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => info!("reconciled {:?}", o),
                Err(e) => warn!("reconcile failed: {:?}", e),
            }
        })
        .await;

    info!("controller stopped gracefully");
    Ok(())
}
