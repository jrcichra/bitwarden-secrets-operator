pub mod bitwarden;
pub mod prometheus;
use crate::bitwarden::BitwardenSecret;
use axum::{routing::get, Router};
use clap::Parser;
use kube::{Client, CustomResourceExt};
use std::{fs::File, io::Write, process};
use tokio::net::TcpListener;
use tracing::info;

#[derive(Parser, Debug, Clone)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
    #[clap(long, default_value = "kubernetes")]
    folder: String,
    #[clap(long, default_value_t = 60 * 5)]
    reconcile_interval: u64,
    #[clap(long, default_value_t = 60 * 2)]
    secret_interval: u64,
    #[clap(long, default_value_t = false)]
    generate_crd: bool,
    #[clap(long, default_value_t = 8000)]
    metrics_port: u16,
}

fn write_file(path: String, content: String) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(content.as_bytes())?;
    Ok(())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let client = Client::try_default().await.unwrap();
    let metrics_port = args.metrics_port;

    if args.generate_crd {
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
        bitwarden::run(client, args, session).await.unwrap();
    });
    info!("starting metrics http server...");

    let app = Router::new().route("/metrics", get(prometheus::gather_metrics));

    let bind = format!("0.0.0.0:{}", metrics_port);
    let listener = TcpListener::bind(&bind).await.unwrap();
    info!("listening on {}", &bind);
    axum::serve(listener, app).await.unwrap();
}
