//! Prometheus metrics, served on `/metrics` by the leader only. A follower
//! exporting `last_reconcile = 0` would trip "hasn't reconciled in an hour"
//! alerts, so `main` starts the server after winning the lease.

use prometheus::{
    IntCounter, IntCounterVec, IntGauge, TextEncoder, register_int_counter,
    register_int_counter_vec, register_int_gauge,
};
use std::sync::LazyLock;

pub static LAST_SUCCESSFUL_RECONCILE: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "last_reconcile",
        "Unix time of the last successful reconcile"
    )
    .expect("register last_reconcile")
});

pub static RECONCILE_FAILURE_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "reconcile_failure_total",
        "Total number of failed reconciles, by Ready condition reason",
        &["reason"]
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

pub static CACHE_REFRESH_FAILURE_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        "cache_refresh_failure_total",
        "Total number of failed Secrets Manager cache refreshes"
    )
    .expect("register cache_refresh_failure_total")
});

/// Register the counters up front, so they export 0 instead of being absent
/// until their first increment. `last_reconcile` stays lazy: a 0 before the
/// first reconcile would read as "stale for 56 years".
pub fn init() {
    for reason in ["NotFound", "InvalidObject", "ApplyFailed"] {
        RECONCILE_FAILURE_TOTAL.with_label_values(&[reason]);
    }
    LazyLock::force(&CACHED_SECRETS);
    LazyLock::force(&CACHE_REFRESH_FAILURE_TOTAL);
}

/// Axum handler for `/metrics`.
pub async fn gather() -> String {
    TextEncoder::new()
        .encode_to_string(&prometheus::gather())
        .unwrap_or_else(|e| format!("# failed to encode metrics: {e}\n"))
}
