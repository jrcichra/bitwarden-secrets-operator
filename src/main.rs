//! Kubernetes operator that syncs Bitwarden Secrets Manager secrets into
//! `Secret`s, driven by `BitwardenSecret` resources.

mod controller;
mod metrics;
mod sm;

use anyhow::{Context, Result};
use axum::{Router, routing::get};
use clap::Parser;
use controller::BitwardenSecret;
use kube::{Client, CustomResourceExt};
use kube_leader_election::{LeaseLock, LeaseLockParams, LeaseLockResult};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::oneshot;
use tracing::{info, warn};

const LEASE_TTL: Duration = Duration::from_secs(15);
const LEASE_RENEW_EVERY: Duration = Duration::from_secs(5);

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Seconds between Secrets Manager refreshes and Secret re-applies.
    #[arg(long, env, default_value_t = 300)]
    reconcile_interval: u64,
    /// Print the CRD as YAML and exit.
    #[arg(long, env)]
    generate_crd: bool,
    /// Port for Prometheus `/metrics`.
    #[arg(long, env, default_value_t = 8000)]
    metrics_port: u16,
    /// Namespace of the leader-election lease (use the downward API).
    #[arg(long, env, required_unless_present = "generate_crd")]
    namespace: Option<String>,
    /// Name of the leader-election `Lease`. Give each install its own when
    /// running several in one namespace.
    #[arg(long, env, default_value = "bitwarden-secrets-operator")]
    lease_name: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    if args.generate_crd {
        print!("{}", serde_yaml::to_string(&BitwardenSecret::crd())?);
        return Ok(());
    }

    // kube and the Bitwarden SDK both use rustls; pin the crypto provider so
    // rustls never has to pick one
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("a rustls crypto provider was already installed"))?;

    // read directly rather than via clap so it never shows up in `Args`' Debug output
    let token = std::env::var("BWS_ACCESS_TOKEN").context("BWS_ACCESS_TOKEN is not set")?;
    let namespace = args.namespace.context("--namespace is required")?;
    let client = Client::try_default().await?;

    let lease = LeaseLock::new(
        client.clone(),
        &namespace,
        LeaseLockParams {
            holder_id: gethostname::gethostname().to_string_lossy().into_owned(),
            lease_name: args.lease_name.clone(),
            lease_ttl: LEASE_TTL,
        },
    );
    info!("waiting for leader lease...");
    while !matches!(
        lease.try_acquire_or_renew().await?,
        LeaseLockResult::Acquired(_)
    ) {
        tokio::time::sleep(LEASE_RENEW_EVERY).await;
    }
    info!("acquired leader lease");

    let sm = sm::SecretsManager::login(token).await?;
    info!("logged in to Bitwarden Secrets Manager");

    let listener = TcpListener::bind(("0.0.0.0", args.metrics_port)).await?;
    info!("serving metrics on :{}", args.metrics_port);
    tokio::spawn(async move {
        let app = Router::new().route("/metrics", get(metrics::gather));
        if let Err(e) = axum::serve(listener, app).await {
            warn!("metrics server stopped: {e}");
        }
    });

    // the controller needs a Sync shutdown future, so watch lease + signals in
    // a task and hand it the receiving end of a oneshot
    let lost_lease = Arc::new(AtomicBool::new(false));
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    tokio::spawn({
        let lost_lease = lost_lease.clone();
        async move {
            tokio::select! {
                () = hold_lease(lease) => lost_lease.store(true, Ordering::SeqCst),
                () = shutdown_signal() => info!("received shutdown signal"),
            }
            let _ = stop_tx.send(());
        }
    });

    controller::run(
        client,
        sm,
        Duration::from_secs(args.reconcile_interval),
        async move {
            let _ = stop_rx.await;
        },
    )
    .await;

    if lost_lease.load(Ordering::SeqCst) {
        // exit non-zero so the restarted container rejoins as a follower
        anyhow::bail!("lost the leader lease");
    }
    Ok(())
}

/// Keep renewing the lease; return once it is lost, or once renewals have
/// failed for a full TTL (another replica may have taken over by then).
async fn hold_lease(lease: LeaseLock) {
    let mut last_renewed = Instant::now();
    loop {
        tokio::time::sleep(LEASE_RENEW_EVERY).await;
        match lease.try_acquire_or_renew().await {
            Ok(LeaseLockResult::Acquired(_)) => last_renewed = Instant::now(),
            Ok(_) => {
                warn!("leader lease taken by another replica");
                return;
            }
            Err(e) if last_renewed.elapsed() >= LEASE_TTL => {
                warn!("could not renew leader lease for {LEASE_TTL:?}: {e}");
                return;
            }
            Err(e) => warn!("lease renewal failed, retrying: {e}"),
        }
    }
}

/// Resolves on SIGTERM (pod shutdown) or SIGINT (Ctrl-C).
async fn shutdown_signal() {
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}
