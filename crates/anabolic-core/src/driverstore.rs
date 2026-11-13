// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The driver packages Windows keeps, in the manner of DriverStore
//! Explorer: every third-party package (oemNN.inf) with its maker, kind,
//! version and date, its size in the driver store, and the devices that
//! use it. Graphics drivers leave a copy of every version installed: the
//! old ones no device uses can go, one at a time; Windows refuses to
//! remove a package a device uses.

#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Package {
    /// The name Windows published it under: oem42.inf.
    pub published: String,
    /// The maker's own name for it: nv_dispi.inf.
    pub original: String,
    pub provider: String,
    pub class: String,
    pub version: String,
    /// mm/dd/yyyy as the INF writes it, and sortable.
    pub date: String,
    pub sortable: String,
    /// Its folder in the driver store, and what it holds.
    pub folder: Option<PathBuf>,
    pub size: u64,
    /// Devices that use it: connected ones, and all.
    pub connected: usize,
    pub devices: usize,
    /// A newer package of the same name is there.
    pub superseded: bool,
}

impl Package {
    /// Can go: no device uses it and a newer version is there.
    pub fn removable(&self) -> bool {
        self.devices == 0 && self.superseded
    }
}

#[derive(Debug, Default)]
pub struct Store {
    pub packages: Vec<Package>,
    pub took: Duration,
}

impl Store {
    /// What the old, unused versions take.
    pub fn reclaimable(&self) -> u64 {
        self.packages
            .iter()
            .filter(|p| p.removable())
            .map(|p| p.size)
            .sum()
    }
}

/// An INF's text, UTF-16 (with its byte order mark) or 8-bit.
pub fn inf_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// [Version]'s Provider, Class and DriverVer, %strings% resolved.
pub fn inf_version(text: &str) -> (String, String, String, String) {
    let mut section = String::new();
    let mut version: Vec<(String, String)> = Vec::new();
    let mut strings: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let line = line.split(';').next().unwrap_or("").trim();
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).to_lowercase();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let pair = (
            k.trim().to_lowercase(),
            v.trim().trim_matches('"').to_string(),
        );
        match section.as_str() {
            "version" => version.push(pair),
            "strings" => strings.push(pair),
            _ => {}
        }
    }
    let get = |k: &str| {
        let v = version
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        match v.strip_prefix('%').and_then(|s| s.strip_suffix('%')) {
            Some(name) => strings
                .iter()
                .find(|(n, _)| *n == name.to_lowercase())
                .map_or(v.clone(), |(_, s)| s.clone()),
            None => v,
        }
    };
    let driver_ver = get("driverver");
    let (date, ver) = driver_ver.split_once(',').unwrap_or((&driver_ver, ""));
    (
        get("provider"),
        get("class"),
        date.trim().to_string(),
        ver.trim().to_string(),
    )
}

/// mm/dd/yyyy as yyyy-mm-dd, to compare.
pub fn sortable(date: &str) -> String {
    let p: Vec<&str> = date.split('/').collect();
    if p.len() == 3 {
        format!("{:0>4}-{:0>2}-{:0>2}", p[2], p[0], p[1])
    } else {
        date.to_string()
    }
}

/// Marks the packages a newer one of the same name replaces.
pub fn mark_superseded(packages: &mut [Package]) {
    let key = |p: &Package| (p.original.to_lowercase(), p.provider.to_lowercase());
    for i in 0..packages.len() {
        let me = key(&packages[i]);
        let mine = (
            packages[i].sortable.clone(),
            version_key(&packages[i].version),
        );
        packages[i].superseded = packages
            .iter()
            .any(|q| key(q) == me && (q.sortable.clone(), version_key(&q.version)) > mine);
    }
}

fn version_key(v: &str) -> Vec<u64> {
    v.split('.')
        .map(|n| n.trim().parse().unwrap_or(0))
        .collect()
}

#[cfg(windows)]
fn folder_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => folder_size(&e.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

#[cfg(windows)]
pub fn read() -> Result<Store, String> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::SetupGetInfDriverStoreLocationW;
    use windows::core::{HSTRING, PCWSTR};

    let started = std::time::Instant::now();
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let inf_dir = Path::new(&root).join("INF");
    let devices = crate::devices::read()
        .map(|d| d.devices)
        .unwrap_or_default();
    let mut packages = Vec::new();
    for entry in std::fs::read_dir(&inf_dir)
        .map_err(|e| e.to_string())?
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if !name.starts_with("oem") || !name.ends_with(".inf") {
            continue;
        }
        let mut buf = vec![0u16; 1024];
        let located = unsafe {
            SetupGetInfDriverStoreLocationW(
                &HSTRING::from(name.as_str()),
                None,
                PCWSTR::null(),
                &mut buf,
                None,
            )
        };
        let stored = located
            .ok()
            .map(|_| PathBuf::from(crate::util::from_wide(&buf)));
        let text = std::fs::read(entry.path())
            .map(|b| inf_text(&b))
            .unwrap_or_default();
        let (provider, class, date, version) = inf_version(&text);
        let using: Vec<&crate::devices::Device> = devices
            .iter()
            .filter(|d| {
                d.driver
                    .as_ref()
                    .and_then(|x| x.inf.as_deref())
                    .is_some_and(|inf| inf.eq_ignore_ascii_case(&name))
            })
            .collect();
        let folder = stored
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        packages.push(Package {
            original: stored
                .as_ref()
                .and_then(|p| p.file_name())
                .map_or(name.clone(), |n| n.to_string_lossy().into_owned()),
            published: name,
            provider,
            class,
            sortable: sortable(&date),
            date,
            version,
            size: folder.as_deref().map_or(0, folder_size),
            folder,
            connected: using
                .iter()
                .filter(|d| d.state != crate::devices::DeviceState::NotConnected)
                .count(),
            devices: using.len(),
            superseded: false,
        });
    }
    mark_superseded(&mut packages);
    packages.sort_by_key(|p| std::cmp::Reverse((p.removable(), p.size)));
    Ok(Store {
        packages,
        took: started.elapsed(),
    })
}

/// Removes a package no device uses; Windows refuses one that a device
/// uses.
#[cfg(windows)]
pub fn remove(published: &str) -> Result<String, String> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::SetupUninstallOEMInfW;
    use windows::core::HSTRING;
    let name = published.trim().to_lowercase();
    if !name.starts_with("oem") || !name.ends_with(".inf") || name.contains(['\\', '/']) {
        return Err(format!("{published} is not a driver package's name"));
    }
    if unsafe { SetupUninstallOEMInfW(&HSTRING::from(name.as_str()), 0, None) }.as_bool() {
        Ok(format!("The driver package {name} is removed."))
    } else {
        Err(format!(
            "{name} could not be removed: {} (a device may still use it, or it needs \
administrator rights)",
            windows::core::Error::from_thread().message()
        ))
    }
}

#[cfg(target_os = "linux")]
pub fn read() -> Result<Store, String> {
    Err("Linux keeps drivers in the kernel's modules, not in a driver store.".into())
}

#[cfg(target_os = "linux")]
pub fn remove(_published: &str) -> Result<String, String> {
    Err("Linux has no driver store.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_inf() {
        let text = "[Version]\r\nSignature=\"$Windows NT$\"\r\nClass=Display\r\n\
                    Provider=%NVIDIA%\r\nDriverVer = 10/01/2026, 32.0.15.6614 ; v\r\n\
                    [Strings]\r\nNVIDIA = \"NVIDIA\"\r\n";
        let (provider, class, date, version) = inf_version(text);
        assert_eq!(
            (
                provider.as_str(),
                class.as_str(),
                date.as_str(),
                version.as_str()
            ),
            ("NVIDIA", "Display", "10/01/2026", "32.0.15.6614")
        );
        assert_eq!(sortable("1/5/2026"), "2026-01-05");
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("[Version]".encode_utf16().flat_map(|u| u.to_le_bytes()))
            .collect();
        assert_eq!(inf_text(&utf16), "[Version]");
    }

    #[test]
    fn knows_the_old_versions() {
        let p = |published: &str, date: &str, devices| Package {
            published: published.into(),
            original: "nv_dispi.inf".into(),
            provider: "NVIDIA".into(),
            sortable: sortable(date),
            date: date.into(),
            devices,
            ..Package::default()
        };
        let mut list = vec![
            p("oem1.inf", "01/01/2026", 0),
            p("oem2.inf", "10/01/2026", 1),
        ];
        mark_superseded(&mut list);
        assert!(list[0].removable());
        assert!(!list[1].removable());
        assert!(remove("oem1.inf; del x").is_err() || cfg!(target_os = "linux"));
    }

    #[cfg(windows)]
    #[test]
    fn reads_this_machines_store() {
        let s = read().unwrap();
        for p in s.packages.iter().take(12) {
            println!(
                "{:<12} {:<28} {:<20} {:<12} {:>10} {:>9} MB  devices {}/{}{}",
                p.published,
                p.original,
                p.provider,
                p.class,
                p.date,
                p.size >> 20,
                p.connected,
                p.devices,
                if p.removable() { "  OLD" } else { "" }
            );
        }
        println!(
            "{} packages, {} MB can go, in {:?}",
            s.packages.len(),
            s.reclaimable() >> 20,
            s.took
        );
    }
}
