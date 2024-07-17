pub mod bitwarden;
pub mod prometheus;
use crate::bitwarden::BitwardenSecret;
use anyhow::Result;
use axum::{routing::get, Router};
use clap::Parser;
use kube::{Client, CustomResourceExt};
use kube_leader_election::{LeaseLock, LeaseLockParams};
use std::{fs::File, io::Write, process, thread, time::Duration};
use tokio::net::TcpListener;
use tracing::{info, warn};

#[derive(Parser, Debug, Clone)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
    #[clap(long, env, default_value = "kubernetes")]
    folder: String,
    #[clap(long, env,default_value_t = 60 * 5)]
    reconcile_interval: u64,
    #[clap(long, env,default_value_t = 60 * 2)]
    secret_interval: u64,
    #[clap(long, env, default_value_t = false)]
    generate_crd: bool,
    #[clap(long, env, default_value_t = 8000)]
    metrics_port: u16,
    #[clap(long, env)]
    namespace: String, // set from downward API
}

fn write_file(path: String, content: String) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(content.as_bytes())?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    ctrlc::set_handler(move || {
        process::exit(0);
    })?;

    let args = Args::parse();
    let hostname = gethostname::gethostname();
    let client = Client::try_default().await?;
    let metrics_port = args.metrics_port;

    if args.generate_crd {
        // Generate and serialize the CRD
        info!("writing crd...");
        write_file(
            "crd.yaml".to_string(),
            serde_yaml::to_string(&BitwardenSecret::crd())?,
        )?;
        info!("done!");
        process::exit(0);
    }

    let leadership = LeaseLock::new(
        kube::Client::try_default().await?,
        &args.namespace,
        LeaseLockParams {
            holder_id: hostname.into_string().unwrap(),
            lease_name: "bitwarden-secrets-operator".into(),
            lease_ttl: Duration::from_secs(15),
        },
    );

    info!("waiting for lock...");
    loop {
        let lease = leadership.try_acquire_or_renew().await?;
        if lease.acquired_lease {
            break;
        }
        thread::sleep(Duration::from_secs(5));
    }
    info!("acquired lock!");

    // start a background thread to see if we're still leader
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let lease = match leadership.try_acquire_or_renew().await {
                Ok(l) => l,
                Err(e) => {
                    warn!("background lease error: {}", e);
                    continue;
                }
            };
            if !lease.acquired_lease {
                info!("lost lease, exiting...");
                process::exit(1);
            }
        }
    });

    // login and get a session key
    info!("login to bitwarden...");
    let session = bitwarden::login().await.unwrap();
    info!("logged in to bitwarden");

    info!("starting bitwarden-secrets-operator...");
    tokio::spawn(async move {
        bitwarden::run(client, args, session).await.unwrap();
    });
    info!("starting metrics http server...");

    let app = Router::new().route("/metrics", get(prometheus::gather_metrics));

    let bind = format!("0.0.0.0:{}", metrics_port);
    let listener = TcpListener::bind(&bind).await?;
    info!("listening on {}", &bind);
    axum::serve(listener, app).await?;
    Ok(())
}
