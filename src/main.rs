#[macro_use]
extern crate rocket;
pub mod bitwarden;
pub mod prometheus;
use std::{fs::File, io::Write, process, time::Duration};

use crate::bitwarden::BitwardenSecret;
use kube::{Client, CustomResourceExt};
use kube_leader_election::{LeaseLock, LeaseLockParams};
use serde::Deserialize;
use tokio::fs;
#[derive(Deserialize, Debug)]
pub struct Configuration {
    #[serde(default = "default_folder")]
    folder: String,
    #[serde(default = "default_reconcile_interval")]
    reconcile_interval: u64,
    #[serde(default = "default_secret_interval")]
    secret_interval: u64,
    #[serde(default = "default_generate_crd")]
    generate_crd: bool,
}

fn default_folder() -> String {
    "kubernetes".to_string()
}

fn default_reconcile_interval() -> u64 {
    60 * 5
}

fn default_secret_interval() -> u64 {
    60 * 2
}

fn default_generate_crd() -> bool {
    false
}

fn write_file(path: String, content: String) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(content.as_bytes())?;
    Ok(())
}

#[launch]
async fn rocket() -> _ {
    tracing_subscriber::fmt::init();
    let client = Client::try_default().await.unwrap();

    let config = envy::prefixed("BITWARDEN_SECRETS_OPERATOR_")
        .from_env::<Configuration>()
        .expect("could not parse configuration");

    if config.generate_crd {
        // Generate and serialize the CRD
        info!("writing crd...");
        write_file(
            "crd.yaml".to_string(),
            serde_yaml::to_string(&BitwardenSecret::crd()).unwrap(),
        )
        .unwrap();
        info!("done!");
        process::exit(0x0100);
    }

    // leader election - block everything until the lease is acquired
    {
        let leadership = LeaseLock::new(
            client.clone(),
            client.default_namespace(),
            LeaseLockParams {
                holder_id: fs::read_to_string("/etc/hostname")
                    .await
                    .unwrap()
                    .trim()
                    .to_string(), // /etc/hostname avoids need for downward api
                lease_name: "bitwarden-secrets-operator".into(),
                lease_ttl: Duration::from_secs(15),
            },
        );
        info!("acquiring lock...");
        while !leadership
            .try_acquire_or_renew()
            .await
            .unwrap()
            .acquired_lease
        {
            info!("retry acquiring lock...");
        }
        info!("lock acquired!");
    }

    // login and get a session key
    let session = bitwarden::login().unwrap();

    info!("starting bitwarden-secrets-operator...");
    tokio::spawn(async move {
        bitwarden::run(client, config, session).await.unwrap();
    });
    info!("starting metrics http server...");
    rocket::build().mount("/", routes![prometheus::gather_metrics])
}
