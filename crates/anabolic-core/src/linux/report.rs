// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Which Linux this is: the distribution from /etc/os-release, the kernel
//! from /proc.

pub fn os_version() -> Option<String> {
    let release = std::fs::read_to_string("/etc/os-release").ok()?;
    let name = release.lines().find_map(|l| {
        l.strip_prefix("PRETTY_NAME=")
            .map(|v| v.trim_matches('"').to_string())
    })?;
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|k| format!(", kernel {}", k.trim()))
        .unwrap_or_default();
    Some(format!("{name}{kernel}"))
}
