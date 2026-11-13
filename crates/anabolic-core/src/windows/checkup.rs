// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The checkup's Windows part: PCIe links and Resizable BAR from the
//! Configuration Manager (the devices' link properties and the memory
//! ranges given to them), virtualization-based security from the kernel,
//! and the rest from the registry and the power service, as the settings
//! pages keep them. Nothing is opened or changed.

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    ALLOC_LOG_CONF, CM_Free_Log_Conf_Handle, CM_Free_Res_Des_Handle, CM_Get_Child,
    CM_Get_First_Log_Conf, CM_Get_Next_Res_Des, CM_Get_Res_Des_Data, CM_Get_Res_Des_Data_Size,
    CM_RESTYPE, CR_SUCCESS, ResType_Mem, ResType_MemLarge,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_CompatibleIds, DEVPKEY_Device_DeviceDesc, DEVPKEY_Device_FriendlyName,
};
use windows::Win32::Foundation::{DEVPROPKEY, ERROR_SUCCESS};
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::{IfOperStatusUp, MediaConnectStateConnected};
use windows::Win32::System::Power::PowerReadACValueIndex;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use windows::core::GUID;

use crate::checkup::{Area, Check, Fix, Link, Slot, Verdict, integrated, link};
use crate::devices::{list, node, number, text};
use crate::reg;
use crate::util::from_wide;

// pciprop.h: the link's speed (1 = 2.5 GT/s … 5 = 32 GT/s) and lanes.
const PCI: GUID = GUID::from_u128(0x3ab22e31_8264_4b4e_9af5_a8d2d8e33e62);
const CURRENT_SPEED: DEVPROPKEY = DEVPROPKEY {
    fmtid: PCI,
    pid: 9,
};
const CURRENT_WIDTH: DEVPROPKEY = DEVPROPKEY {
    fmtid: PCI,
    pid: 10,
};
const MAX_SPEED: DEVPROPKEY = DEVPROPKEY {
    fmtid: PCI,
    pid: 11,
};
const MAX_WIDTH: DEVPROPKEY = DEVPROPKEY {
    fmtid: PCI,
    pid: 12,
};

pub fn checks() -> Vec<Check> {
    let mut out = pci();
    out.extend(security());
    out.extend(gaming());
    out.extend(power());
    out.extend(storage());
    out.extend(ethernet());
    out.extend(mouse());
    out
}

fn name_of(inst: u32) -> String {
    text(inst, &DEVPKEY_Device_FriendlyName)
        .or_else(|| text(inst, &DEVPKEY_Device_DeviceDesc))
        .unwrap_or_else(|| "Device".into())
}

/// The largest memory range a device was given: a graphics card's window
/// onto its own memory (its BAR), 256 MB without Resizable BAR and all of
/// it with.
fn largest_range(inst: u32) -> Option<u64> {
    let mut conf = 0usize;
    if unsafe { CM_Get_First_Log_Conf(Some(&mut conf), inst, ALLOC_LOG_CONF) } != CR_SUCCESS {
        return None;
    }
    let mut largest = 0u64;
    for kind in [ResType_Mem, ResType_MemLarge] {
        largest = largest.max(ranges(conf, kind));
    }
    let _ = unsafe { CM_Free_Log_Conf_Handle(conf) };
    (largest > 0).then_some(largest)
}

/// The largest range of one resource kind in a configuration.
fn ranges(conf: usize, kind: CM_RESTYPE) -> u64 {
    let mut largest = 0u64;
    let mut at = conf;
    let mut owned = false;
    loop {
        let mut next = 0usize;
        if unsafe { CM_Get_Next_Res_Des(&mut next, at, kind, None, 0) } != CR_SUCCESS {
            break;
        }
        if owned {
            let _ = unsafe { CM_Free_Res_Des_Handle(at) };
        }
        at = next;
        owned = true;
        let mut size = 0u32;
        if unsafe { CM_Get_Res_Des_Data_Size(&mut size, at, 0) } != CR_SUCCESS || size < 24 {
            continue;
        }
        let mut buf = vec![0u8; size as usize];
        if unsafe { CM_Get_Res_Des_Data(at, buf.as_mut_ptr().cast(), size, 0) } != CR_SUCCESS {
            continue;
        }
        // MEM_DES and MEM_LARGE_DES alike: count, type, base, end.
        let base = u64::from_le_bytes(buf[8..16].try_into().unwrap_or_default());
        let end = u64::from_le_bytes(buf[16..24].try_into().unwrap_or_default());
        if end > base {
            largest = largest.max(end - base + 1);
        }
    }
    if owned {
        let _ = unsafe { CM_Free_Res_Des_Handle(at) };
    }
    largest
}

fn pcie_link(inst: u32) -> Link {
    Link {
        width: number(inst, &CURRENT_WIDTH).unwrap_or(0),
        max_width: number(inst, &MAX_WIDTH).unwrap_or(0),
        generation: number(inst, &CURRENT_SPEED).unwrap_or(0),
        max_generation: number(inst, &MAX_SPEED).unwrap_or(0),
    }
}

/// Graphics cards (class 03) and NVMe drives (class 01, 08): their links,
/// and a card's Resizable BAR.
fn pci() -> Vec<Check> {
    let mut out = Vec::new();
    let Ok(ids) = crate::devices::instance_ids(Some("PCI")) else {
        return out;
    };
    let (mut gpus, mut drives) = (0, 0);
    for id in ids {
        let Some(inst) = node(&id) else {
            continue;
        };
        let compatible = list(inst, &DEVPKEY_Device_CompatibleIds);
        let class = |c: &str| {
            compatible
                .iter()
                .any(|x| x.to_uppercase().contains(&format!("CC_{c}")))
        };
        if class("0300") || class("0302") {
            let name = name_of(inst);
            if integrated(&name) {
                continue;
            }
            out.extend(link(Slot::Graphics, gpus, &name, pcie_link(inst)));
            out.extend(rebar(gpus, &name, largest_range(inst)));
            gpus += 1;
        } else if class("010802") {
            // The controller is "Standard NVM Express Controller"; the drive
            // hangs off it under its own name.
            let mut child = 0u32;
            let name = if unsafe { CM_Get_Child(&mut child, inst, 0) } == CR_SUCCESS {
                name_of(child)
            } else {
                name_of(inst)
            };
            out.extend(link(Slot::Drive, drives, &name, pcie_link(inst)));
            drives += 1;
        }
    }
    out
}

fn size(bytes: u64) -> String {
    const GB: u64 = 1 << 30;
    if bytes >= GB {
        format!("{} GB", (bytes as f64 / GB as f64).round())
    } else {
        format!("{} MB", bytes >> 20)
    }
}

fn rebar(index: usize, name: &str, window: Option<u64>) -> Option<Check> {
    let window = window?;
    const GB: u64 = 1 << 30;
    let key = format!("rebar-{index}");
    let title = format!("{name}: Resizable BAR");
    Some(if window >= GB {
        Check::new(
            key,
            Area::Graphics,
            title,
            Verdict::Good,
            format!(
                "On: the processor sees {} of the card's memory at once",
                size(window)
            ),
            "",
        )
    } else {
        Check::new(
            key,
            Area::Graphics,
            title,
            Verdict::Consider,
            format!(
                "Off: the processor sees the card's memory through a {} window",
                size(window)
            ),
            "With Resizable BAR the processor can write to all of the card's memory at once. \
Some games gain a few percent and a few lose (NVIDIA's driver turns it on per game). It needs \
Above 4G decoding in the firmware, and Windows installed in UEFI mode.",
        )
        .fix(Fix::Firmware(
            "Turn on Above 4G Decoding and Re-Size BAR Support (under PCI or PCIe settings)."
                .into(),
        ))
    })
}

/// Virtualization-based security and memory integrity (HVCI), as running
/// now: SYSTEM_ISOLATED_USER_MODE_INFORMATION's first byte.
fn security() -> Option<Check> {
    let mut info = [0u8; 16];
    let mut returned = 0u32;
    let status = unsafe {
        crate::ntapi::NtQuerySystemInformation(
            crate::ntapi::SystemIsolatedUserModeInformation,
            info.as_mut_ptr().cast(),
            info.len() as u32,
            &mut returned,
        )
    };
    if !crate::ntapi::nt_success(status) {
        return None;
    }
    let vbs = info[0] & 1 != 0;
    let hvci = info[0] & 2 != 0;
    let (now, why) = if hvci {
        (
            "On (with virtualization-based security)",
            "It keeps malicious and vulnerable drivers out of the kernel by checking their code \
in an isolated part of memory. The checks cost a little: Microsoft's own advice for gaming PCs \
mentions turning it off while playing and back on afterwards. It is a security trade, not a \
fault.",
        )
    } else if vbs {
        (
            "Off; virtualization-based security is on",
            "Drivers' code is not checked in isolated memory: a vulnerable driver can be used to \
reach the kernel. Windows Security turns it on; an old driver can stop it.",
        )
    } else {
        (
            "Off",
            "Drivers' code is not checked in isolated memory: a vulnerable driver can be used to \
reach the kernel. Windows Security turns it on; an old driver can stop it.",
        )
    };
    Some(
        Check::new(
            "memory-integrity",
            Area::System,
            "Memory integrity",
            Verdict::Info,
            now,
            why,
        )
        .fix(Fix::open("Core isolation", "windowsdefender://coreisolation")),
    )
}

const GAME_DVR: &str = r"Software\Microsoft\Windows\CurrentVersion\GameDVR";

fn gaming() -> Vec<Check> {
    let mut out = Vec::new();
    let allowed = reg::number_at(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Policies\Microsoft\Windows\GameDVR",
        "AllowGameDVR",
    ) != Some(0);
    let capture = reg::number_at(HKEY_CURRENT_USER, GAME_DVR, "AppCaptureEnabled") != Some(0);
    let background =
        reg::number_at(HKEY_CURRENT_USER, GAME_DVR, "HistoricalCaptureEnabled") == Some(1);
    let captures = Fix::open("Captures settings", "ms-settings:gaming-gamedvr");
    out.push(if allowed && capture && background {
        Check::new(
            "background-recording",
            Area::Graphics,
            "Background recording",
            Verdict::Change,
            "On: the last minutes of every game are recorded",
            "Xbox Game Bar keeps recording so that it can save what just happened: the card's \
video encoder runs all the time, and some games lose a few frames per second.",
        )
        .fix(captures)
    } else {
        Check::new(
            "background-recording",
            Area::Graphics,
            "Background recording",
            Verdict::Good,
            "Off",
            "",
        )
    });

    let game_mode = reg::number_at(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\GameBar",
        "AutoGameModeEnabled",
    ) != Some(0);
    out.push(if game_mode {
        Check::new(
            "game-mode",
            Area::Graphics,
            "Windows Game Mode",
            Verdict::Good,
            "On",
            "",
        )
    } else {
        Check::new(
            "game-mode",
            Area::Graphics,
            "Windows Game Mode",
            Verdict::Consider,
            "Off",
            "During play it keeps Windows Update from installing drivers and showing restart \
notices, and gives the game first claim on the processor.",
        )
        .fix(Fix::open("Game Mode", "ms-settings:gaming-gamemode"))
    });

    let graphics = Fix::open("Graphics settings", "ms-settings:display-advancedgraphics");
    match crate::gpu::gpu_scheduling() {
        Some((_, true)) => out.push(Check::new(
            "gpu-scheduling",
            Area::Graphics,
            "Hardware-accelerated GPU scheduling",
            Verdict::Good,
            "On",
            "The card schedules its own work. Frame generation (DLSS 3 and later) needs it.",
        )),
        Some((_, false)) => out.push(
            Check::new(
                "gpu-scheduling",
                Area::Graphics,
                "Hardware-accelerated GPU scheduling",
                Verdict::Consider,
                "Off",
                "Frame generation (DLSS 3 and later) needs it on; otherwise the difference is small either way. It takes a restart.",
            )
            .fix(graphics),
        ),
        None => {}
    }
    out
}

/// Windows 11's power mode on top of the plan.
const BEST_EFFICIENCY: GUID = GUID::from_u128(0x961cc777_2547_4f9d_8174_7d86181b8a7a);
const BEST_PERFORMANCE: GUID = GUID::from_u128(0xded574b5_45a0_4f42_8737_46345c09c238);
const NO_SUBGROUP: GUID = GUID::from_u128(0xfea3413e_7e05_4911_9a71_700331f1c294);
const PERSONALITY: GUID = GUID::from_u128(0x245d8541_3943_4422_b025_13a784f679b7);
const USB_SUBGROUP: GUID = GUID::from_u128(0x2a737441_1930_4402_8d77_b2bebba308a3);
const USB_SUSPEND: GUID = GUID::from_u128(0x48e6b7a6_50f5_4782_a5d4_53bb8f07e226);

#[link(name = "powrprof", kind = "raw-dylib")]
unsafe extern "system" {
    fn PowerGetEffectiveOverlayScheme(overlay: *mut GUID) -> u32;
}

fn power() -> Vec<Check> {
    let mut out = Vec::new();
    let settings = || Fix::open("Power settings", "ms-settings:powersleep");
    let Some(plan) = crate::power::active() else {
        return out;
    };
    let name = crate::power::name(&plan);
    let mut overlay = GUID::zeroed();
    let overlay = (unsafe { PowerGetEffectiveOverlayScheme(&mut overlay) } == 0).then_some(overlay);
    let mode = match overlay {
        Some(g) if g == BEST_EFFICIENCY => ", power mode Best power efficiency",
        Some(g) if g == BEST_PERFORMANCE => ", power mode Best performance",
        _ => "",
    };
    // A plan's kind, whatever its name and GUID: copies of Ultimate
    // Performance get a GUID of their own. 0 saver, 1 high, 2 balanced.
    let mut personality = 2u32;
    let _ = unsafe {
        PowerReadACValueIndex(
            None,
            Some(&plan),
            Some(&NO_SUBGROUP),
            Some(&PERSONALITY),
            &mut personality,
        )
    };
    out.push(if personality == 0 {
        Check::new(
            "power-plan",
            Area::Power,
            "Power plan",
            Verdict::Change,
            format!("{name}{mode}"),
            "The processor is held to low clocks to save energy.",
        )
        .fix(settings())
    } else if overlay == Some(BEST_EFFICIENCY) {
        Check::new(
            "power-plan",
            Area::Power,
            "Power plan",
            Verdict::Consider,
            format!("{name}{mode}"),
            "Windows favours saving energy over speed: the processor boosts later and less.",
        )
        .fix(settings())
    } else if personality == 1 {
        Check::new(
            "power-plan",
            Area::Power,
            "Power plan",
            Verdict::Info,
            format!("{name}{mode}"),
            "The cores never rest deeply: idle power and heat are higher. Recent processors \
boost as high under Balanced.",
        )
    } else {
        Check::new(
            "power-plan",
            Area::Power,
            "Power plan",
            Verdict::Good,
            format!("{name}{mode}"),
            "",
        )
    });

    let mut usb = 0u32;
    if unsafe {
        PowerReadACValueIndex(
            None,
            Some(&plan),
            Some(&USB_SUBGROUP),
            Some(&USB_SUSPEND),
            &mut usb,
        )
    } == ERROR_SUCCESS
    {
        out.push(Check::new(
            "usb-suspend",
            Area::Power,
            "USB selective suspend",
            Verdict::Info,
            if usb == 1 { "On" } else { "Off" },
            "Idle USB devices are put to sleep. A device that wakes slowly can drop out for a \
moment: a known cause of crackling USB audio and of a mouse that stalls on some hubs.",
        ));
    }

    let fast = reg::number_at(
        HKEY_LOCAL_MACHINE,
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Power",
        "HiberbootEnabled",
    ) == Some(1)
        && reg::number_at(
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Control\Power",
            "HibernateEnabled",
        ) != Some(0);
    out.push(
        Check::new(
            "fast-startup",
            Area::Power,
            "Fast startup",
            Verdict::Info,
            if fast { "On" } else { "Off" },
            if fast {
                "Shut down saves the kernel and drivers to disk and starts from them: a \
driver's trouble survives a shut down, and the uptime does not reset. Restart starts clean."
            } else {
                "Shut down ends everything; starting takes a little longer."
            },
        )
        .fix(Fix::Hand(
            "Control Panel > Power Options > Choose what the power buttons do".into(),
        )),
    );

    let pending = reg::open(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending",
        false,
    )
    .is_some()
        || reg::open(
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired",
            false,
        )
        .is_some();
    out.push(if pending {
        Check::new(
            "restart-pending",
            Area::System,
            "Restart",
            Verdict::Consider,
            "Updates wait for a restart",
            "Installed updates take effect at the next restart; until then some are half done, \
and Windows may restart by itself outside active hours.",
        )
        .fix(Fix::Hand("Restart when it suits you.".into()))
    } else {
        Check::new(
            "restart-pending",
            Area::System,
            "Restart",
            Verdict::Good,
            "No restart pending",
            "",
        )
    });
    out
}

fn storage() -> Option<Check> {
    let off = reg::number_at(
        HKEY_LOCAL_MACHINE,
        r"SYSTEM\CurrentControlSet\Control\FileSystem",
        "DisableDeleteNotification",
    ) == Some(1);
    Some(if off {
        Check::new(
            "trim",
            Area::Storage,
            "TRIM",
            Verdict::Change,
            "Off",
            "SSDs are not told which blocks are free: writing slows down over time and the \
drives wear faster.",
        )
        .fix(Fix::Hand(
            "In a terminal run as administrator: fsutil behavior set DisableDeleteNotify 0".into(),
        ))
    } else {
        Check::new("trim", Area::Storage, "TRIM", Verdict::Good, "On", "")
    })
}

/// The speeds a network adapter's driver offers ("1.0 Gbps Full Duplex",
/// "2.5 Gbps Full Duplex"), in bits per second.
pub(crate) fn offered_speed(text: &str) -> Option<u64> {
    let t = text.to_lowercase();
    let bytes = t.as_bytes();
    // "Gb" or "Mb", and the number just before it.
    (1..bytes.len()).find_map(|i| {
        let unit = match (bytes[i - 1], bytes[i]) {
            (b'g', b'b') => 1e9,
            (b'm', b'b') => 1e6,
            _ => return None,
        };
        let head = t[..i - 1].trim_end();
        let start = head
            .rfind(|c: char| !(c.is_ascii_digit() || c == '.'))
            .map_or(0, |at| at + 1);
        let n: f64 = head[start..].parse().ok()?;
        Some((n * unit) as u64)
    })
}

const NET_CLASS: &str =
    r"SYSTEM\CurrentControlSet\Control\Class\{4d36e972-e325-11ce-bfc1-08002be10318}";

/// The adapter's driver settings key, by its interface GUID.
fn adapter_key(guid: &GUID) -> Option<String> {
    let want = format!("{guid:?}").to_lowercase();
    let class = reg::open(HKEY_LOCAL_MACHINE, NET_CLASS, false)?;
    reg::subkeys(&class).into_iter().find_map(|sub| {
        let path = format!(r"{NET_CLASS}\{sub}");
        let id = reg::text_at(HKEY_LOCAL_MACHINE, &path, "NetCfgInstanceId")?;
        (id.trim_matches(['{', '}']).to_lowercase() == want).then_some(path)
    })
}

/// Wired adapters held far below what they can do: a gigabit port at 100
/// Mbps is a cable missing wires, nearly always.
fn ethernet() -> Vec<Check> {
    let mut out = Vec::new();
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    if unsafe { GetIfTable2(&mut table) }.is_err() || table.is_null() {
        return out;
    }
    let rows = unsafe {
        std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize)
    };
    for row in rows {
        let flags = row.InterfaceAndOperStatusFlags._bitfield;
        let hardware = flags & 1 != 0 && flags & 2 == 0;
        // IF_TYPE_ETHERNET_CSMACD
        if !hardware
            || row.Type != 6
            || row.OperStatus != IfOperStatusUp
            || row.MediaConnectState != MediaConnectStateConnected
        {
            continue;
        }
        let name = from_wide(&row.Description);
        let speed = row.ReceiveLinkSpeed.max(row.TransmitLinkSpeed);
        let Some(key) = adapter_key(&row.InterfaceGuid) else {
            continue;
        };
        let offered: Vec<(String, String)> = reg::open(
            HKEY_LOCAL_MACHINE,
            &format!(r"{key}\Ndi\Params\*SpeedDuplex\enum"),
            false,
        )
        .map(|k| {
            reg::values(&k)
                .into_iter()
                .map(|(n, v)| (n, v.text()))
                .collect()
        })
        .unwrap_or_default();
        let most = offered
            .iter()
            .filter_map(|(_, t)| offered_speed(t))
            .max()
            .unwrap_or(0);
        let set = reg::text_at(HKEY_LOCAL_MACHINE, &key, "*SpeedDuplex");
        let forced = set
            .as_deref()
            .filter(|s| *s != "0")
            .and_then(|s| offered.iter().find(|(n, _)| n == s))
            .map(|(_, t)| t.clone());
        let words = |bps: u64| {
            if bps >= 1_000_000_000 {
                format!("{} Gbps", bps as f64 / 1e9)
            } else {
                format!("{} Mbps", bps / 1_000_000)
            }
        };
        let key_name = format!("ethernet-{}", out.len());
        let title = format!("{name}: link");
        out.push(if speed <= 100_000_000 && most >= 1_000_000_000 {
            match forced {
                Some(t) => Check::new(
                    key_name,
                    Area::Network,
                    title,
                    Verdict::Change,
                    format!("{}; set by hand to {t}", words(speed)),
                    "The adapter is held to this speed by its own setting.",
                )
                .fix(Fix::Hand(
                    "Device Manager > the adapter > Advanced > Speed & Duplex: Auto Negotiation"
                        .into(),
                )),
                None => Check::new(
                    key_name,
                    Area::Network,
                    title,
                    Verdict::Change,
                    format!("{} on a {} adapter", words(speed), words(most)),
                    "A gigabit link needs all eight wires of the cable: a damaged cable or plug, \
an old four-wire cable or an old switch port holds it to 100 Mbps.",
                )
                .fix(Fix::Hand(
                    "Try another cable, or another port on the router or switch.".into(),
                )),
            }
        } else {
            Check::new(
                key_name,
                Area::Network,
                title,
                Verdict::Good,
                if most > speed {
                    format!("{} (the adapter does {})", words(speed), words(most))
                } else {
                    words(speed)
                },
                "",
            )
        });
    }
    unsafe { FreeMibTable(table.cast()) };
    out
}

fn mouse() -> Option<Check> {
    let on = reg::text_at(HKEY_CURRENT_USER, r"Control Panel\Mouse", "MouseSpeed")?.trim() != "0";
    Some(
        Check::new(
            "pointer-precision",
            Area::Input,
            "Enhance pointer precision",
            Verdict::Info,
            if on { "On" } else { "Off" },
            "With it on, the pointer goes farther when the mouse moves fast: handy on the \
desktop, but aim in games that use the pointer changes with speed, so many players turn it \
off.",
        )
        .fix(Fix::open("Mouse settings", "ms-settings:mousetouchpad")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_offered_speeds() {
        assert_eq!(offered_speed("1.0 Gbps Full Duplex"), Some(1_000_000_000));
        assert_eq!(offered_speed("2.5Gbps Full Duplex"), Some(2_500_000_000));
        assert_eq!(offered_speed("100 Mbps Half Duplex"), Some(100_000_000));
        assert_eq!(offered_speed("10 Gbps Full Duplex"), Some(10_000_000_000));
        assert_eq!(offered_speed("1 Gb Full"), Some(1_000_000_000));
        assert_eq!(offered_speed("Auto Negotiation"), None);
    }

    #[test]
    #[ignore = "reads this machine's settings; run by hand"]
    fn checks_this_machine() {
        let c = crate::checkup::read();
        for x in &c.checks {
            println!(
                "{:?} {:?} {} = {} | {:?}",
                x.verdict, x.area, x.title, x.now, x.fix
            );
        }
        println!("{} checks in {:?}", c.checks.len(), c.took);
    }
}
