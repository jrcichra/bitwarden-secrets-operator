#[macro_use]
extern crate rocket;
pub mod bitwarden;
pub mod prometheus;
use kube::Client;
use kube_leader_election::{LeaseLock, LeaseLockParams};
use serde::Deserialize;
use std::{error::Error, time::Duration};
use tokio::fs;
#[derive(Deserialize, Debug)]
pub struct Configuration {
    #[serde(default = "default_folder")]
    folder: String,
    #[serde(default = "default_reconcile_interval")]
    reconcile_interval: u64,
    #[serde(default = "default_secret_interval")]
    secret_interval: u64,
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

// start of code once leader election is completed
async fn start() {
    let client = Client::try_default().await.unwrap();
    let config = envy::prefixed("BITWARDEN_SECRETS_OPERATOR_")
        .from_env::<Configuration>()
        .expect("could not parse configuration");

    // login and get a session key
    let session = bitwarden::login().unwrap();
    info!("starting bitwarden-secrets-operator...");
    tokio::spawn(async move {
        bitwarden::run(client, config, session).await.unwrap();
    });
    info!("starting metrics http server...");
    tokio::spawn(async move {
        rocket::build()
            .mount("/", routes![prometheus::gather_metrics])
            .launch()
            .await
            .unwrap();
    });
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();

    // the death of a thread should kill the process
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_panic(info);
        std::process::exit(1);
    }));

    // leader election - block everything until the lease is acquired
    tokio::spawn(async move {
        let client = Client::try_default().await.unwrap();
        let namespace = client.default_namespace().to_owned();
        let leadership = LeaseLock::new(
            client,
            &namespace,
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
        let mut started = false;
        loop {
            let lease = leadership.try_acquire_or_renew().await.unwrap();
            if lease.acquired_lease {
                if !started {
                    started = true;
                    start().await;
                }
            } else {
                panic!("lost leader election");
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });

    // terribly hold the main thread by sleeping forever
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
