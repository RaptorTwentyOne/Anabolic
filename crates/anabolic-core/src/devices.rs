// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The machine's devices, as Device Manager knows them on Windows and sysfs
//! on Linux: which work, which have a problem and what it is in words,
//! which are turned off, which are no longer connected, and the driver each
//! runs on (who made it, its version and date, its file). It only reads.

use std::time::Duration;

#[cfg_attr(windows, path = "windows/devices.rs")]
#[cfg_attr(target_os = "linux", path = "linux/devices.rs")]
mod imp;
pub use imp::read;
#[cfg(windows)]
pub(crate) use imp::{instance_ids, list, node, number, resources, text};

#[derive(Clone, Debug, Default)]
pub struct Device {
    /// The system's name for it: PCI\VEN_10DE&DEV_2684\4&… on Windows, the
    /// sysfs path on Linux.
    pub id: String,
    pub name: String,
    /// Its kind as Device Manager groups them: Display, Net, USB, …
    pub class: String,
    pub manufacturer: Option<String>,
    /// Where it sits: "PCI bus 1, device 0, function 0", a port.
    pub location: Option<String>,
    pub state: DeviceState,
    /// Windows hides it in Device Manager unless asked to show hidden
    /// devices: system plumbing, mostly.
    pub hidden: bool,
    /// The driver's status code for a problem (an NTSTATUS), when it gave
    /// one.
    pub status_code: Option<u32>,
    pub driver: Option<Driver>,
    /// First installed, last connected and last removed, as FILETIMEs.
    pub installed: Option<i64>,
    pub arrived: Option<i64>,
    pub removed: Option<i64>,
    /// The device it hangs off.
    pub parent: Option<String>,
    /// The name the device gives itself, where it differs from Windows'
    /// own ("G502 HERO Gaming Mouse" for a "USB Input Device").
    pub product: Option<String>,
}

impl Device {
    /// Its own name and Windows' together, when they differ.
    pub fn full_name(&self) -> String {
        match &self.product {
            Some(p) if !self.name.to_lowercase().contains(&p.to_lowercase()) => {
                format!("{p} ({})", self.name)
            }
            _ => self.name.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Driver {
    pub provider: Option<String>,
    pub version: Option<String>,
    /// As a FILETIME.
    pub date: Option<i64>,
    /// The INF file it was installed from (oem12.inf), or on Linux the
    /// kernel module.
    pub inf: Option<String>,
    /// The driver's service: the name its .sys file runs under.
    pub service: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceState {
    /// Something is wrong: the problem's number (Device Manager's "Code").
    Problem(u32),
    /// Turned off by someone.
    Disabled,
    /// No driver is installed for it.
    NoDriver,
    #[default]
    Working,
    /// Known to the system but not connected now.
    NotConnected,
}

impl DeviceState {
    pub fn label(self) -> String {
        match self {
            DeviceState::Problem(code) => format!("Problem (code {code})"),
            DeviceState::Disabled => "Turned off".into(),
            DeviceState::NoDriver => "No driver".into(),
            DeviceState::Working => "Working".into(),
            DeviceState::NotConnected => "Not connected".into(),
        }
    }

    /// Worth a look: a problem or no driver.
    pub fn bad(self) -> bool {
        matches!(self, DeviceState::Problem(_) | DeviceState::NoDriver)
    }
}

#[derive(Debug, Default)]
pub struct Devices {
    pub devices: Vec<Device>,
    pub notes: Vec<String>,
    pub took: Duration,
}

/// What a device problem's code (Device Manager's "Code 43") means, and
/// what usually helps.
pub fn problem_words(code: u32) -> Option<(&'static str, &'static str)> {
    Some(match code {
        1 => ("It is not set up correctly.", "Update its driver."),
        3 => (
            "Its driver may be damaged, or the system is short of memory.",
            "Close some programs; reinstall the driver.",
        ),
        10 => (
            "It cannot start.",
            "Update its driver; the driver's own error is below when Windows has it.",
        ),
        12 => (
            "It cannot find enough free resources to use.",
            "Turn off another device, or change the firmware's settings.",
        ),
        14 => ("It needs the computer to restart.", "Restart the computer."),
        16 => (
            "Windows cannot tell all the resources it uses.",
            "Its properties in Device Manager can set them.",
        ),
        18 => (
            "Its drivers need to be installed again.",
            "Reinstall its driver.",
        ),
        19 => (
            "Its settings in the registry are incomplete or damaged.",
            "Uninstall it in Device Manager and scan for hardware changes.",
        ),
        21 => (
            "Windows is removing it.",
            "Wait a moment, then refresh; restart if it stays.",
        ),
        22 => ("It is turned off.", "Turn it on in Device Manager."),
        24 => (
            "It is not present, not working, or not all its drivers are installed.",
            "Check that it is connected; reinstall its driver.",
        ),
        28 => ("Its drivers are not installed.", "Install its driver."),
        29 => (
            "The firmware turned it off.",
            "Turn it on in the firmware's (BIOS/UEFI) settings.",
        ),
        31 => (
            "Windows cannot load the drivers it needs.",
            "Update or reinstall its driver.",
        ),
        32 => (
            "Its driver's service is turned off.",
            "Set the driver's start type back, or reinstall the driver.",
        ),
        33 => (
            "Windows cannot tell which resources it needs.",
            "The hardware may be faulty; contact its maker.",
        ),
        34 => (
            "Windows cannot set it up on its own.",
            "Set its resources by hand, per its documentation.",
        ),
        35 => (
            "The firmware does not describe it fully.",
            "Update the computer's firmware.",
        ),
        36 => (
            "It asks for an interrupt the firmware gave to another kind.",
            "Change the interrupt settings in the firmware.",
        ),
        37 => (
            "Its driver failed as it started.",
            "Reinstall or update its driver.",
        ),
        38 => (
            "A copy of its driver from before is still in memory.",
            "Restart the computer.",
        ),
        39 => ("Its driver is damaged or missing.", "Reinstall its driver."),
        40 => (
            "Its driver's service is missing or damaged in the registry.",
            "Reinstall its driver.",
        ),
        41 => (
            "Its driver loaded, but the hardware was not found.",
            "Check that it is connected; reinstall its driver.",
        ),
        42 => ("Two copies of it are present.", "Restart the computer."),
        43 => (
            "Windows stopped it: it reported a problem.",
            "Update its driver; reconnect or replace it if it keeps happening.",
        ),
        44 => (
            "A program or service shut it down.",
            "Restart the computer.",
        ),
        45 => (
            "It is not connected.",
            "Connect it, or uninstall it if it is gone for good.",
        ),
        46 => (
            "Windows is shutting down.",
            "It comes back at the next start.",
        ),
        47 => (
            "It was prepared for safe removal.",
            "Unplug it and plug it in again.",
        ),
        48 => (
            "Its driver is blocked: it is known to cause problems.",
            "Get a newer driver from its maker.",
        ),
        49 => (
            "The system's registry is too large to start more devices.",
            "Uninstall devices that are no longer used.",
        ),
        50 => (
            "Windows cannot set all its properties.",
            "Contact its maker for an updated driver.",
        ),
        51 => (
            "It is waiting for another device to start.",
            "Fix the device it waits for.",
        ),
        52 => (
            "Its driver's signature cannot be verified.",
            "Install a driver signed for this version of Windows.",
        ),
        53 => (
            "The kernel debugger keeps it.",
            "Turn kernel debugging off.",
        ),
        54 => (
            "It failed and is being reset.",
            "Wait; restart if it stays.",
        ),
        56 => (
            "Windows is still setting it up.",
            "Wait; restart if it stays.",
        ),
        57 => (
            "Its driver's properties could not be applied.",
            "Reinstall its driver.",
        ),
        _ => return None,
    })
}

/// A USB device someone plugged in: not a hub, a controller or the
/// composite parent of another.
pub fn plugged(d: &Device) -> bool {
    let id = d.id.to_uppercase();
    let usb = id.starts_with(r"USB\")
        || id.starts_with(r"USBSTOR\")
        || (id.starts_with("/SYS/") && id.contains("/USB"));
    let name = d.name.to_lowercase();
    usb && !d.hidden
        && ![
            "hub",
            "host controller",
            "composite device",
            "root hub",
            "host router",
        ]
        .iter()
        .any(|n| name.contains(n))
}

/// Every USB device this PC has known, in the manner of USBDeview: the
/// last connected first, whether it is still there or not.
pub fn usb_history(devices: &[Device]) -> Vec<&Device> {
    // To the second: a device's interfaces arrive a moment apart.
    let secs = |d: &Device| d.arrived.or(d.installed).unwrap_or(0) / 10_000_000;
    let mut out: Vec<&Device> = devices.iter().filter(|d| plugged(d)).collect();
    out.sort_by_cached_key(|d| (std::cmp::Reverse(secs(d)), d.full_name()));
    // A device's interfaces (a mouse's buttons, its keys) under one name.
    out.dedup_by(|a, b| a.full_name() == b.full_name() && secs(a) == secs(b));
    out
}

/// The devices worth a look first: problems, then those with no driver.
pub fn troubled(devices: &[Device]) -> Vec<&Device> {
    let mut out: Vec<&Device> = devices.iter().filter(|d| d.state.bad()).collect();
    out.sort_by_key(|d| (d.state, d.name.to_lowercase()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_what_was_plugged_in() {
        let usb = |id: &str, name: &str, arrived: i64| Device {
            id: id.into(),
            name: name.into(),
            arrived: Some(arrived),
            ..Device::default()
        };
        let list = [
            usb(r"USB\VID_046D&PID_C52B\5&1", "USB Receiver", 10),
            usb(r"USB\ROOT_HUB30\4&1", "USB Root Hub (USB 3.0)", 30),
            usb(r"USBSTOR\DISK&VEN_SANDISK\1", "SanDisk Cruzer", 20),
            usb(r"PCI\VEN_10DE&DEV_2C02\4&1", "NVIDIA GeForce", 40),
        ];
        let names: Vec<&str> = usb_history(&list).iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["SanDisk Cruzer", "USB Receiver"]);
    }

    #[test]
    fn puts_problems_first() {
        let device = |name: &str, state| Device {
            name: name.into(),
            state,
            ..Device::default()
        };
        let list = [
            device("b", DeviceState::Working),
            device("a", DeviceState::NoDriver),
            device("c", DeviceState::Problem(43)),
            device("d", DeviceState::Disabled),
        ];
        let names: Vec<&str> = troubled(&list).iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["c", "a"]);
        assert_eq!(
            problem_words(43).unwrap().0,
            "Windows stopped it: it reported a problem."
        );
        assert_eq!(DeviceState::Problem(10).label(), "Problem (code 10)");
    }
}
