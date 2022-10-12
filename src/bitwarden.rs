use super::prometheus;
use futures::StreamExt;
use k8s_openapi::api::core::v1::Secret;
use kube::Resource;
use kube::{
    api::{Api, ListParams, ObjectMeta, Patch, PatchParams},
    runtime::controller::{Action, Controller},
    Client, CustomResource,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fs;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

use crate::Configuration;

#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(group = "jrcichra.dev", version = "v1", kind = "BitwardenSecret")]
#[kube(shortname = "bws", namespaced)]
struct BitwardenSecretSpec {
    name: String,
}
// Data we want access to in error/reconcile calls
struct Data {
    client: Client,
    config: Configuration,
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
    let config = &ctx.config;
    let name = &generator.spec.name;
    let mut contents = BTreeMap::new();

    // get the session id every reconcile loop
    let session = get_session();

    // build the content for the secret here
    match get_secrets(&session, &config.folder) {
        Ok(secrets) => match secrets.get(name) {
            Some(value) => match value.get("login") {
                Some(login) => {
                    // set the username and password keys
                    contents.insert(
                        "username".to_string(),
                        String::from(login["username"].as_str().unwrap()),
                    );
                    contents.insert(
                        "password".to_string(),
                        String::from(login["password"].as_str().unwrap()),
                    );
                }
                None => match value.get("notes") {
                    Some(notes) => {
                        // set it with key "notes"
                        contents.insert("notes".to_string(), String::from(notes.as_str().unwrap()));
                    }
                    None => {
                        return Err(ReconcileError::BitwardenError(format!(
                            "card/login not found for {}",
                            name
                        )));
                    }
                },
            },
            None => {
                return Err(ReconcileError::BitwardenError(format!(
                    "{} not found",
                    name
                )));
            }
        },
        Err(e) => {
            return Err(ReconcileError::BitwardenError(e.to_string()));
        }
    }

    let oref = generator.controller_owner_ref(&()).unwrap();
    let secret = Secret {
        metadata: ObjectMeta {
            name: generator.metadata.name.clone(),
            owner_references: Some(vec![oref]),
            ..ObjectMeta::default()
        },
        string_data: Some(contents),
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
        return Err(format!("stdout: {}\nstderr: {}", stdout, stderr).into());
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
        return Err(format!("stdout: {}\nstderr: {}", stdout, stderr).into());
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
        return Err(format!("stdout: {}\nstderr: {}", stdout, stderr).into());
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

fn get_session() -> String {
    let homedir = dirs::home_dir().unwrap();
    let path = format!(
        "{}/{}",
        homedir.to_str().unwrap(),
        ".config/Bitwarden CLI/session"
    );
    String::from(fs::read_to_string(path).unwrap().trim())
}

pub async fn run(client: Client, config: Configuration) -> Result<(), Box<dyn Error>> {
    let cmgs = Api::<BitwardenSecret>::all(client.clone());
    let cms = Api::<BitwardenSecret>::all(client.clone());

    let (mut reload_tx, reload_rx) = futures::channel::mpsc::channel(0);

    // Reconcile loop timer
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(config.interval));
        loop {
            interval.tick().await;
            info!(
                "interval of {} seconds triggering reconcile loop",
                config.interval
            );
            reload_tx
                .try_send(())
                .expect("could not trigger reconcile loop");
        }
    });

    Controller::new(cmgs, ListParams::default())
        .owns(cms, ListParams::default())
        .reconcile_all_on(reload_rx.map(|_| ()))
        .shutdown_on_signal()
        .run(reconcile, error_policy, Arc::new(Data { client, config }))
        .for_each(|res| async move {
            match res {
                Ok(o) => info!("reconciled {:?}", o),
                Err(e) => warn!("reconcile failed: {}", e),
            }
        })
        .await;
    Ok(())
}
