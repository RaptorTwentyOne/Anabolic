// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Linux keeps such limits per control group, and systemd owns those: a
//! process moved out of its unit's group escapes the unit (stopping a
//! service would no longer end it). Anabolic does not move processes
//! between them.

use super::{Applied, Limits};
use crate::actions::Target;

const WHY: &str = "Linux limits whole control groups, which systemd owns: \
                   `systemctl set-property <unit> CPUQuota=25% MemoryMax=4G` limits a service";

/// Limits cannot be set here: why.
pub fn available() -> Result<(), String> {
    Err(WHY.to_string())
}

pub fn limit(group: &str, targets: &[Target], limits: Limits) -> Applied {
    Applied {
        group: group.to_string(),
        limits,
        before: None,
        joined: Vec::new(),
        failed: targets
            .iter()
            .map(|t| (t.clone(), WHY.to_string()))
            .collect(),
        error: Some(WHY.to_string()),
    }
}

pub fn lift(group: &str) -> Applied {
    Applied {
        group: group.to_string(),
        limits: Limits::default(),
        before: None,
        joined: Vec::new(),
        failed: Vec::new(),
        error: None,
    }
}

pub fn lift_all() {}
