// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What changed on Windows. From the event logs: Windows Update's
//! installs and failures (WindowsUpdateClient 19 and 20), Windows
//! Installer's products installed and removed (MsiInstaller 1033 and
//! 1034), services and kernel drivers set up (Service Control Manager
//! 7045), and drivers installed or updated on a device (Kernel-PnP 400 in
//! its Configuration log, inbox drivers left out unless they replaced
//! another). From the registry: the install date programs give in their
//! uninstall entries, for the installers that log nothing.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use windows::Win32::Devices::Properties::{DEVPKEY_Device_DeviceDesc, DEVPKEY_Device_FriendlyName};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

use crate::changes::{Change, Changes, Kind, yyyymmdd};
use crate::evtlog::{Event, query};
use crate::reg;

const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";

fn data(e: &Event, i: usize) -> String {
    e.data
        .get(i)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| v != "(NULL)")
        .unwrap_or_default()
}

fn device_name(id: &str, names: &mut HashMap<String, Option<String>>) -> Option<String> {
    names
        .entry(id.to_string())
        .or_insert_with(|| {
            // A device known but not connected keeps its name too.
            let wide = crate::util::wide(id);
            let mut inst = 0u32;
            let found = unsafe {
                windows::Win32::Devices::DeviceAndDriverInstallation::CM_Locate_DevNodeW(
                    &mut inst,
                    windows::core::PCWSTR(wide.as_ptr()),
                    windows::Win32::Devices::DeviceAndDriverInstallation::CM_LOCATE_DEVNODE_PHANTOM,
                )
            } == windows::Win32::Devices::DeviceAndDriverInstallation::CR_SUCCESS;
            found
                .then(|| {
                    crate::devices::text(inst, &DEVPKEY_Device_FriendlyName)
                        .or_else(|| crate::devices::text(inst, &DEVPKEY_Device_DeviceDesc))
                })
                .flatten()
        })
        .clone()
}

fn from_events(out: &mut Vec<Change>, notes: &mut Vec<String>) {
    let list = "<QueryList><Query Id='0'>\
        <Select Path='System'>*[System[\
        (Provider[@Name='Microsoft-Windows-WindowsUpdateClient'] and (EventID=19 or EventID=20)) or \
        (Provider[@Name='Service Control Manager'] and EventID=7045)]]</Select>\
        <Select Path='Application'>*[System[\
        (Provider[@Name='MsiInstaller'] and (EventID=1033 or EventID=1034))]]</Select>\
        <Select Path='Microsoft-Windows-Kernel-PnP/Configuration'>*[System[EventID=400]]</Select>\
        </Query></QueryList>";
    let events = match query(list) {
        Ok(e) => e,
        Err(e) => {
            notes.push(format!("The event logs could not be read: {e}"));
            return;
        }
    };
    let mut names = HashMap::new();
    // A driver package installs on several devices at once: one line.
    let mut drivers: HashSet<(String, i64)> = HashSet::new();
    for e in &events {
        let change = |kind, what: String, detail: String| Change {
            at: e.at,
            day_only: false,
            kind,
            what,
            detail,
            times: 1,
        };
        match (e.provider.as_str(), e.id) {
            ("Microsoft-Windows-WindowsUpdateClient", 19) => out.push(change(
                Kind::Update,
                e.get("updateTitle").unwrap_or("An update").to_string(),
                String::new(),
            )),
            ("Microsoft-Windows-WindowsUpdateClient", 20) => out.push(change(
                Kind::UpdateFailed,
                e.get("updateTitle").unwrap_or("An update").to_string(),
                format!("error {}", e.get("errorCode").unwrap_or("?")),
            )),
            ("Service Control Manager", 7045) => {
                let kind = e.get("ServiceType").unwrap_or("");
                out.push(change(
                    Kind::Service,
                    e.get("ServiceName").unwrap_or("A service").to_string(),
                    format!(
                        "{}, {}: {}",
                        if kind.contains("kernel") || kind.contains("file system") {
                            "kernel driver"
                        } else {
                            "service"
                        },
                        e.get("StartType").unwrap_or(""),
                        e.get("ImagePath").unwrap_or("").trim_matches('"')
                    ),
                ))
            }
            ("MsiInstaller", id @ (1033 | 1034)) => out.push(change(
                if id == 1033 {
                    Kind::Program
                } else {
                    Kind::ProgramRemoved
                },
                data(e, 0),
                [data(e, 1), data(e, 4)]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(", "),
            )),
            ("Microsoft-Windows-Kernel-PnP", 400) => {
                let inf = e.get("DriverName").unwrap_or("null");
                let inbox = e.get("DriverInbox") == Some("true");
                let updated = e.get("DeviceUpdated") == Some("true");
                if inf == "null" || (inbox && !updated) {
                    continue;
                }
                let package = e.get("DriverPackageId").unwrap_or(inf).to_string();
                // The same package within the same minute is one install.
                if !drivers.insert((package, e.at / 600_000_000)) {
                    continue;
                }
                let device = e
                    .get("DeviceInstanceId")
                    .and_then(|id| device_name(id, &mut names))
                    .unwrap_or_else(|| inf.to_string());
                out.push(change(
                    Kind::Driver,
                    device,
                    format!(
                        "{} {} ({}){}",
                        e.get("DriverProvider").unwrap_or(""),
                        e.get("DriverVersion").unwrap_or(""),
                        inf,
                        if updated {
                            ", replacing the one before"
                        } else {
                            ""
                        }
                    ),
                ));
            }
            _ => {}
        }
    }
}

/// Programs' own install dates, for those no event names.
fn from_uninstall(out: &mut Vec<Change>) {
    let mut known: HashSet<String> = out
        .iter()
        .filter(|c| c.kind == Kind::Program)
        .map(|c| c.what.to_lowercase())
        .collect();
    for (root, wow32) in [
        (HKEY_LOCAL_MACHINE, false),
        (HKEY_LOCAL_MACHINE, true),
        (HKEY_CURRENT_USER, false),
    ] {
        let Some(key) = reg::open(root, UNINSTALL, wow32) else {
            continue;
        };
        for sub in reg::subkeys(&key) {
            let Some(k) = reg::open(root, &format!(r"{UNINSTALL}\{sub}"), wow32) else {
                continue;
            };
            let (Some(name), Some(date)) =
                (reg::text(&k, "DisplayName"), reg::text(&k, "InstallDate"))
            else {
                continue;
            };
            // Parts of others and updates have their own entries.
            if reg::number(&k, "SystemComponent") == Some(1)
                || reg::text(&k, "ParentKeyName").is_some()
            {
                continue;
            }
            let Some(at) = yyyymmdd(&date) else {
                continue;
            };
            if !known.insert(name.to_lowercase()) {
                continue;
            }
            out.push(Change {
                at,
                day_only: true,
                kind: Kind::Program,
                what: name,
                detail: [reg::text(&k, "DisplayVersion"), reg::text(&k, "Publisher")]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(", "),
                times: 1,
            });
        }
    }
}

pub fn read() -> Result<Changes, String> {
    let started = Instant::now();
    let mut changes = Vec::new();
    let mut notes = Vec::new();
    from_events(&mut changes, &mut notes);
    from_uninstall(&mut changes);
    changes.sort_by_key(|c| std::cmp::Reverse(c.at));
    let changes = crate::changes::collapse(changes);
    if let Some(since) = crate::evtlog::oldest("System") {
        notes.push(format!(
            "The event logs reach back to {}; programs' own install dates go further.",
            crate::sysinfo::local_time(since)
                .map(|t| t.chars().take(10).collect::<String>())
                .unwrap_or_default()
        ));
    }
    Ok(Changes {
        changes,
        notes,
        took: started.elapsed(),
    })
}
