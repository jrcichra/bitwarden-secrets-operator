use prometheus::{self, Encoder, IntCounter, IntGauge, TextEncoder};

use lazy_static::lazy_static;
use prometheus::{register_int_counter, register_int_gauge};

lazy_static! {
    pub static ref LAST_SUCCESSFUL_RECONCILE: IntGauge =
        register_int_gauge!("last_reconcile", "Time we last reconciled successfully").unwrap();
    pub static ref RECONCILE_SUCCESS_TOTAL: IntCounter = register_int_counter!(
        "reconcile_success_total",
        "Total number of successful reconciles"
    )
    .unwrap();
    pub static ref RECONCILE_FAILURE_TOTAL: IntCounter = register_int_counter!(
        "reconcile_failure_total",
        "Total number of failed reconciles"
    )
    .unwrap();
    pub static ref CACHED_SECRETS: IntGauge = register_int_gauge!(
        "cached_secrets",
        "Number of secrets currently held in the in-memory cache"
    )
    .unwrap();
    pub static ref CACHE_REFRESH_SUCCESS_TOTAL: IntCounter = register_int_counter!(
        "cache_refresh_success_total",
        "Total number of successful Bitwarden cache refreshes"
    )
    .unwrap();
    pub static ref CACHE_REFRESH_FAILURE_TOTAL: IntCounter = register_int_counter!(
        "cache_refresh_failure_total",
        "Total number of failed Bitwarden cache refreshes"
    )
    .unwrap();
}

pub async fn gather_metrics() -> String {
    let metric_families = prometheus::gather();
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}
