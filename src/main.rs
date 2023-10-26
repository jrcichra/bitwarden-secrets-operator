pub mod bitwarden;
pub mod prometheus;
use std::{fs::File, io::Write, net::SocketAddr, process};

use crate::bitwarden::BitwardenSecret;
use axum::{routing::get, Router};
use kube::{Client, CustomResourceExt};
use serde::Deserialize;
use tracing::info;
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
    #[serde(default = "default_metrics_port")]
    metrics_port: u16,
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

fn default_metrics_port() -> u16 {
    8000
}

fn write_file(path: String, content: String) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(content.as_bytes())?;
    Ok(())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let client = Client::try_default().await.unwrap();

    let config = envy::prefixed("BITWARDEN_SECRETS_OPERATOR_")
        .from_env::<Configuration>()
        .expect("could not parse configuration");
    let metrics_port = config.metrics_port;

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

    // login and get a session key
    let session = bitwarden::login().unwrap();

    info!("starting bitwarden-secrets-operator...");
    tokio::spawn(async move {
        bitwarden::run(client, config, session).await.unwrap();
    });
    info!("starting metrics http server...");

    let app = Router::new().route("/metrics", get(prometheus::gather_metrics));
    let addr = SocketAddr::from(([0, 0, 0, 0], metrics_port));
    info!("listening on {}", addr);
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}
