//! The resolver's cache: systemd-resolved shows it to root only, through a
//! debugging interface, so names come from the lookups seen instead.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

pub fn names() -> HashMap<IpAddr, Arc<str>> {
    HashMap::new()
}
