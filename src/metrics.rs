//! Prometheus metrics, served on `/metrics` by the leader only. A follower
//! exporting `last_reconcile = 0` would trip "hasn't reconciled in an hour"
//! alerts, so `main` starts the server after winning the lease.

use prometheus::{IntCounter, IntGauge, TextEncoder, register_int_counter, register_int_gauge};
use std::sync::LazyLock;

pub static LAST_SUCCESSFUL_RECONCILE: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "last_reconcile",
        "Unix time of the last successful reconcile"
    )
    .expect("register last_reconcile")
});

pub static RECONCILE_SUCCESS_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        "reconcile_success_total",
        "Total number of successful reconciles"
    )
    .expect("register reconcile_success_total")
});

pub static RECONCILE_FAILURE_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        "reconcile_failure_total",
        "Total number of failed reconciles"
    )
    .expect("register reconcile_failure_total")
});

pub static CACHED_SECRETS: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "cached_secrets",
        "Number of secrets currently held in the in-memory cache"
    )
    .expect("register cached_secrets")
});

pub static CACHE_REFRESH_SUCCESS_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        "cache_refresh_success_total",
        "Total number of successful Secrets Manager cache refreshes"
    )
    .expect("register cache_refresh_success_total")
});

pub static CACHE_REFRESH_FAILURE_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        "cache_refresh_failure_total",
        "Total number of failed Secrets Manager cache refreshes"
    )
    .expect("register cache_refresh_failure_total")
});

/// Axum handler for `/metrics`.
pub async fn gather() -> String {
    TextEncoder::new()
        .encode_to_string(&prometheus::gather())
        .unwrap_or_else(|e| format!("# failed to encode metrics: {e}\n"))
}
