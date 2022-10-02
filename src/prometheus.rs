use prometheus::{self, Encoder, IntGauge, TextEncoder};

use lazy_static::lazy_static;
use prometheus::register_int_gauge;

lazy_static! {
    pub static ref LAST_SUCCESSFUL_RECONCILE: IntGauge =
        register_int_gauge!("last_reconcile", "Time we last reconciled sucessfully").unwrap();
}

#[get("/metrics")]
pub fn gather_metrics() -> String {
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    let encoder = TextEncoder::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer.clone()).unwrap()
}
