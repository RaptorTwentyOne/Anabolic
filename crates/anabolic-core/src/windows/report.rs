// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Which Windows this is, from the registry as winver reads it. Its
//! ProductName still says "Windows 10" on Windows 11: the build tells.

use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;

use crate::reg;

const CURRENT: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

pub fn os_version() -> Option<String> {
    let name = reg::text_at(HKEY_LOCAL_MACHINE, CURRENT, "ProductName")?;
    let build: u32 = reg::text_at(HKEY_LOCAL_MACHINE, CURRENT, "CurrentBuild")?
        .trim()
        .parse()
        .ok()?;
    let name = if build >= 22000 {
        name.replace("Windows 10", "Windows 11")
    } else {
        name
    };
    let release = reg::text_at(HKEY_LOCAL_MACHINE, CURRENT, "DisplayVersion")
        .map_or(String::new(), |v| format!(" {v}"));
    let revision = reg::number_at(HKEY_LOCAL_MACHINE, CURRENT, "UBR")
        .map_or(String::new(), |r| format!(".{r}"));
    Some(format!("{name}{release} (build {build}{revision})"))
}
