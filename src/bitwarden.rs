use super::prometheus;
use crate::Configuration;
use chrono;
use futures::StreamExt;
use k8s_openapi::api::core::v1::Secret;
use kube::runtime::watcher;
use kube::Resource;
use kube::{
    api::{Api, ObjectMeta, Patch, PatchParams},
    runtime::controller::{Action, Controller},
    Client, CustomResource,
};
use lazy_static::lazy_static;
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

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
    cache: Arc<Mutex<HashMap<String, serde_json::Value>>>,
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
    // build the content for the secret here
    match ctx.cache.lock().unwrap().get(name) {
        Some(value) => match value.get("login") {
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
                match value.get(notes_constant) {
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
        },
        None => {
            return Err(ReconcileError::BitwardenError(format!(
                "{} not found",
                name
            )));
        }
    }

    let oref = generator.controller_owner_ref(&()).unwrap();
    let current_time = chrono::offset::Utc::now();
    let mut annotations = BTreeMap::new();
    annotations.insert("lastReconciled".to_string(), current_time.to_rfc3339());
    let secret = Secret {
        metadata: ObjectMeta {
            name: generator.metadata.name.clone(),
            owner_references: Some(vec![oref]),
            annotations: Some(annotations),
            ..ObjectMeta::default()
        },
        string_data: Some(contents),
        type_: type_.clone(),
        ..Default::default()
    };
    let secret_api = Api::<Secret>::namespaced(
        client.clone(),
        generator
            .metadata
            .namespace
            .as_ref()
            .ok_or(ReconcileError::MissingObjectKey(".metadata.namespace"))?,
    );
    secret_api
        .patch(
            secret
                .metadata
                .name
                .as_ref()
                .ok_or(ReconcileError::MissingObjectKey(".metadata.name"))?,
            &PatchParams::apply("bitwarden-secrets-operator.jrcichra.dev"),
            &Patch::Apply(&secret),
        )
        .await
        .map_err(ReconcileError::SecretCreationFailed)?;

    // let prometheus know we started the reconcile loop
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    prometheus::LAST_SUCCESSFUL_RECONCILE.set(now.as_secs().try_into().unwrap());

    Ok(Action::requeue(Duration::from_secs(300)))
}

// collect secrets and return a hash of the results
fn get_secrets(
    session: &str,
    folder: &str,
) -> Result<HashMap<std::string::String, serde_json::Value>, Box<dyn Error>> {
    let mut secrets = HashMap::new();
    // sync the secrets
    let res = Command::new("bw")
        .arg("sync")
        .arg("--session")
        .arg(&session)
        .output()?;
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
        .output()?;
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
        .output()?;
    let stdout = String::from_utf8_lossy(&res.stdout).to_string();
    let stderr = String::from_utf8_lossy(&res.stderr).to_string();
    if !res.status.success() {
        return Err(format!("list items failed: stdout: {}\nstderr: {}", stdout, stderr).into());
    }

    // parse stdout
    let v: Value = serde_json::from_str(&stdout)?;

    // loop through each item
    if let Some(arr) = v.as_array() {
        for item in arr {
            secrets.insert(String::from(item["name"].as_str().unwrap()), item.clone());
        }
    }

    Ok(secrets)
}

/// The controller triggers this on reconcile errors
fn error_policy(_object: Arc<BitwardenSecret>, _error: &ReconcileError, _ctx: Arc<Data>) -> Action {
    Action::requeue(Duration::from_secs(30))
}

pub fn login() -> Result<String, Box<dyn Error>> {
    // logout in case already logged in
    Command::new("bw").arg("logout").output()?;

    // first login with --apikey
    let res = Command::new("bw").arg("login").arg("--apikey").output()?;
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
        .output()?;
    let stdout = String::from_utf8_lossy(&res.stdout).to_string();
    let stderr = String::from_utf8_lossy(&res.stderr).to_string();

    if !res.status.success() {
        return Err(format!("unlock failed: stdout: {}\nstderr: {}", stdout, stderr).into());
    }

    // the session key is within the stdout of this command
    lazy_static! {
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
    config: Configuration,
    session: String,
) -> Result<(), Box<dyn Error>> {
    let cmgs = Api::<BitwardenSecret>::all(client.clone());
    let cms = Api::<BitwardenSecret>::all(client.clone());
    let cache = Arc::new(Mutex::new(HashMap::new()));

    let (mut reload_tx, reload_rx) = futures::channel::mpsc::channel(0);

    // Reconcile loop timer
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(config.reconcile_interval));
        loop {
            interval.tick().await;
            info!(
                "interval of {} seconds triggering reconcile loop",
                config.reconcile_interval
            );
            reload_tx
                .try_send(())
                .expect("could not trigger reconcile loop");
        }
    });
    let cache_gather = Arc::clone(&cache);
    let folder = config.folder.clone();
    let session_clone = session.clone();
    // Secret Gatherer timer - independent of reconciliation since it grabs all secrets at once
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(config.secret_interval));
        loop {
            interval.tick().await;
            info!(
                "interval of {} seconds triggering secret gather loop",
                config.secret_interval
            );
            match get_secrets(&session, &folder) {
                Ok(secrets) => {
                    // update the cache
                    cache_gather.lock().unwrap().clone_from(&secrets);
                }
                Err(e) => {
                    warn!("secret gatherer failed {:?}", e)
                }
            }
        }
    });
    // run secret grabber once at the start
    match get_secrets(&session_clone, &config.folder) {
        Ok(secrets) => {
            // update the cache
            cache.lock().unwrap().clone_from(&secrets);
        }
        Err(e) => {
            warn!("secret gatherer failed {:?}", e)
        }
    }

    Controller::new(cmgs, watcher::Config::default())
        .owns(cms, watcher::Config::default())
        .reconcile_all_on(reload_rx.map(|_| ()))
        .shutdown_on_signal()
        .run(reconcile, error_policy, Arc::new(Data { client, cache }))
        .for_each(|res| async move {
            match res {
                Ok(o) => info!("reconciled {:?}", o),
                Err(e) => warn!("reconcile failed: {:?}", e),
            }
        })
        .await;
    Ok(())
}
