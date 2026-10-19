//! systemd's services.

use std::collections::HashMap;
use std::sync::Arc;

use super::*;

pub(super) fn list(_configs: &mut HashMap<Arc<str>, Option<Arc<Config>>>) -> ServiceList {
    ServiceList {
        error: Some("not available on Linux yet".into()),
        ..Default::default()
    }
}

pub(super) fn control(
    _name: &str,
    display: &str,
    _op: ServiceOp,
) -> (Result<String, String>, Vec<String>) {
    (
        Err(format!(
            "Could not control {display}: not available on Linux yet"
        )),
        Vec::new(),
    )
}
