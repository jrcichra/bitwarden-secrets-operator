use super::prometheus;
use crate::Args;
use anyhow::Result;
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
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::process::Command;
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

// Data we want access to in error/reconcile calls
struct Data {
    client: Client,
    cache: Arc<Mutex<HashMap<String, Value>>>,
}

#[derive(Debug, Error)]
enum ReconcileError {
    #[error("Failed to create Secret: {0}")]
    SecretCreationFailed(#[source] kube::Error),
    #[error("MissingObjectKey: {0}")]
    MissingObjectKey(&'static str),
    #[error("BitwardenError: {0}")]
    BitwardenError(String),
}

// collect secrets and return a hash of the results
async fn get_secrets(
    session: &str,
    folder: &str,
) -> Result<HashMap<std::string::String, serde_json::Value>, Box<dyn Error>> {
    let mut secrets = HashMap::new();
    info!("loading secrets from folder '{}'", folder);
    // sync the secrets
    let res = Command::new("bw")
        .arg("sync")
        .arg("--session")
        .arg(&session)
        .output()
        .await?;
    let stdout = String::from_utf8_lossy(&res.stdout).to_string();
    let stderr = String::from_utf8_lossy(&res.stderr).to_string();
    if !res.status.success() {
        return Err(format!("sync failed: stdout: {}\nstderr: {}", stdout, stderr).into());
    }

    // get the id of the provided folder
    let res = Command::new("bw")
        .arg("get")
        .arg("folder")
        .arg(&folder)
        .arg("--session")
        .arg(&session)
        .output()
        .await?;
    let stdout = String::from_utf8_lossy(&res.stdout).to_string();
    let stderr = String::from_utf8_lossy(&res.stderr).to_string();
    if !res.status.success() {
        return Err(format!("get folder failed: stdout: {}\nstderr: {}", stdout, stderr).into());
    }

    // parse stdout
    let folder_json: Value = serde_json::from_str(&stdout)?;
    // get the secrets in the provided folder
    let res = Command::new("bw")
        .arg("list")
        .arg("items")
        .arg("--folderid")
        .arg(folder_json["id"].as_str().unwrap())
        .arg("--session")
        .arg(&session)
        .output()
        .await?;
    let stdout = String::from_utf8_lossy(&res.stdout).to_string();
    let stderr = String::from_utf8_lossy(&res.stderr).to_string();
    if !res.status.success() {
        return Err(format!("list items failed: stdout: {}\nstderr: {}", stdout, stderr).into());
    }

    // parse stdout
    let v: Value = serde_json::from_str(&stdout)?;

    // collect all secrets into the hashmap
    if let Some(arr) = v.as_array() {
        for item in arr {
            if let Some(name) = item["name"].as_str() {
                secrets.insert(name.to_string(), item.clone());
            }
        }
    }

    info!("loaded {} secrets from folder '{}'", secrets.len(), folder);
    Ok(secrets)
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
            contents.insert(
                "username".to_string(),
                login["username"].as_str().unwrap().to_string(),
            );
            contents.insert(
                "password".to_string(),
                login["password"].as_str().unwrap().to_string(),
            );
        }
        None => {
            let notes_constant = "notes";
            let mut use_key = notes_constant;
            if let Some(key) = key {
                use_key = key.as_str();
            }
            match secret_value.get(notes_constant) {
                Some(notes) => {
                    contents.insert(use_key.to_string(), notes.as_str().unwrap().to_string());
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
            &PatchParams::apply("bitwarden-secrets-operator.jrcichra.dev"),
            &Patch::Merge(&patch),
        )
        .await
        .map_err(ReconcileError::SecretCreationFailed)?;

    // let prometheus know we started the reconcile loop
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    prometheus::LAST_SUCCESSFUL_RECONCILE.set(now.as_secs().try_into().unwrap());

    // Wait for next change - cache is refreshed periodically in background
    Ok(Action::await_change())
}

/// The controller triggers this on reconcile errors
fn error_policy(object: Arc<BitwardenSecret>, error: &ReconcileError, _ctx: Arc<Data>) -> Action {
    let name = object
        .metadata
        .name
        .as_ref()
        .unwrap_or(&"unknown".to_string())
        .clone();

    warn!(
        "reconcile failed for {}: {:?}",
        name, error
    );

    // Use exponential backoff with max of 5 minutes
    let backoff = calculate_backoff(1);
    Action::requeue(backoff)
}

pub async fn login() -> Result<String, Box<dyn Error>> {
    // logout in case already logged in
    Command::new("bw").arg("logout").output().await?;

    // first login with --apikey
    let res = Command::new("bw")
        .arg("login")
        .arg("--apikey")
        .output()
        .await?;
    let stdout = String::from_utf8_lossy(&res.stdout).to_string();
    let stderr = String::from_utf8_lossy(&res.stderr).to_string();

    if !res.status.success() {
        return Err(format!("login failed: stdout: {}\nstderr: {}", stdout, stderr).into());
    }

    // now unlock the vault, referencing the master password in an env (from a mounted secret, hopefully)
    // TODO: this may hang if BW_PASSWORD is not set
    let res = Command::new("bw")
        .arg("unlock")
        .arg("--passwordenv")
        .arg("BW_PASSWORD")
        .output()
        .await?;
    let stdout = String::from_utf8_lossy(&res.stdout).to_string();
    let stderr = String::from_utf8_lossy(&res.stderr).to_string();

    if !res.status.success() {
        return Err(format!("unlock failed: stdout: {}\nstderr: {}", stdout, stderr).into());
    }

    // the session key is within the stdout of this command
    lazy_static::lazy_static! {
        static ref RE: Regex = Regex::new("export BW_SESSION=\"(.*)\"").unwrap();
    }

    // the first match should be what we need
    for cap in RE.captures_iter(&stdout) {
        return Ok(cap[1].to_string());
    }

    Err(format!("could not find BW_SESSION in unlock output").into())
}

pub async fn run(
    client: Client,
    args: Args,
    session: String,
    cache: Arc<Mutex<HashMap<String, Value>>>,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), Box<dyn Error>> {
    let bitwarden_secrets = Api::<BitwardenSecret>::all(client.clone());

    // Initial cache load
    info!("loading secrets into cache...");
    match get_secrets(&session, &args.folder).await {
        Ok(new_secrets) => {
            let mut cache_guard = cache.lock().unwrap();
            *cache_guard = new_secrets;
            info!("loaded {} secrets into cache", cache_guard.len());
        }
        Err(e) => {
            warn!("failed to load initial secrets: {}", e);
        }
    }

    // Spawn background task to refresh cache periodically
    let session_clone = session.clone();
    let folder_clone = args.folder.clone();
    let cache_clone = cache.clone();
    let reconcile_interval = Duration::from_secs(args.reconcile_interval);
    
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(reconcile_interval).await;
            
            match get_secrets(&session_clone, &folder_clone).await {
                Ok(new_secrets) => {
                    let mut cache_guard = cache_clone.lock().unwrap();
                    *cache_guard = new_secrets;
                    info!("refreshed cache with {} secrets", cache_guard.len());
                }
                Err(e) => {
                    warn!("failed to refresh secrets cache: {}", e);
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
