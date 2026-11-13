// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The resolver's cache: systemd-resolved shows it to root only, through a
//! debugging interface, so names come from the lookups seen instead.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

pub fn names() -> HashMap<IpAddr, Arc<str>> {
    HashMap::new()
}

/// systemd-resolved shows its cache to root only.
pub fn entries() -> Vec<crate::dnscache::Cached> {
    Vec::new()
}

/// Empties systemd-resolved's cache.
pub fn flush() -> Result<(), String> {
    let out = std::process::Command::new("resolvectl")
        .arg("flush-caches")
        .output()
        .map_err(|e| format!("resolvectl could not be run: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}
