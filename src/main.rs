pub mod bitwarden;
pub mod prometheus;
use crate::bitwarden::BitwardenSecret;
use anyhow::Result;
use axum::{routing::get, Router};
use clap::Parser;
use kube::{Client, CustomResourceExt};
use kube_leader_election::{LeaseLock, LeaseLockParams, LeaseLockResult};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fs::File, io::Write, process, thread, time::Duration};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tracing::{info, warn};

#[derive(Parser, Debug, Clone)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
    #[clap(long, env, default_value = "kubernetes")]
    folder: String,
    #[clap(long, env, default_value_t = 60 * 10)]
    reconcile_interval: u64,
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

    let shutdown_requested = Arc::new(AtomicBool::new(false));
    let shutdown_requested_ctrlc = shutdown_requested.clone();

    ctrlc::set_handler(move || {
        info!("received SIGINT, shutting down gracefully...");
        shutdown_requested_ctrlc.store(true, Ordering::SeqCst);
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
        if matches!(lease, LeaseLockResult::Acquired(_)) {
            break;
        }
        thread::sleep(Duration::from_secs(5));
    }
    info!("acquired lock!");

    // Create channels for graceful shutdown coordination
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown_triggered = Arc::new(AtomicBool::new(false));
    let shutdown_triggered_leader = shutdown_triggered.clone();

    // start a background task to see if we're still leader
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            
            if shutdown_triggered_leader.load(Ordering::SeqCst) {
                break;
            }
            
            let lease = match leadership.try_acquire_or_renew().await {
                Ok(l) => l,
                Err(e) => {
                    warn!("background lease error: {}", e);
                    continue;
                }
            };
            if matches!(lease, LeaseLockResult::NotAcquired(_)) {
                info!("lost lease, triggering graceful shutdown...");
                let _ = shutdown_tx.send(());
                break;
            }
        }
    });

    // login and get a session key
    info!("login to bitwarden...");
    let session = bitwarden::login().await.unwrap();
    info!("logged in to bitwarden");

    info!("starting bitwarden-secrets-operator...");

    let controller_handle = tokio::spawn(async move {
        bitwarden::run(client, args, session, shutdown_rx).await.unwrap();
    });

    info!("starting metrics http server...");

    let app = Router::new().route("/metrics", get(prometheus::gather_metrics));

    let bind = format!("0.0.0.0:{}", metrics_port);
    let listener = TcpListener::bind(&bind).await?;
    info!("listening on {}", &bind);
    
    // Run both the controller and metrics server concurrently with shutdown support
    tokio::select! {
        result = axum::serve(listener, app) => {
            result?;
        }
        _ = controller_handle => {
            info!("controller task completed");
        }
        _ = async {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if shutdown_requested.load(Ordering::SeqCst) {
                    break;
                }
            }
        } => {
            info!("shutdown requested via SIGINT");
        }
    }
    
    Ok(())
}
