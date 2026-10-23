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
