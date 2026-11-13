// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Devices from sysfs: the PCI devices (named from the system's pci.ids
//! when it has one) and the USB devices (named by themselves), with the
//! kernel driver bound to each and its module's version. A PCI device of a
//! kind that needs a driver (storage, network, display, sound, USB and the
//! like) and has none is shown as such; bridges and the like need none.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use crate::devices::{Device, DeviceState, Devices, Driver};

pub fn read() -> Result<Devices, String> {
    let started = Instant::now();
    let names = pci_names();
    let mut devices = Vec::new();
    let mut notes = Vec::new();
    match fs::read_dir("/sys/bus/pci/devices") {
        Ok(entries) => {
            for e in entries.flatten() {
                if let Some(d) = pci(&e.path(), &names) {
                    devices.push(d);
                }
            }
        }
        Err(e) => notes.push(format!("/sys/bus/pci could not be read: {e}")),
    }
    if let Ok(entries) = fs::read_dir("/sys/bus/usb/devices") {
        for e in entries.flatten() {
            if let Some(d) = usb(&e.path()) {
                devices.push(d);
            }
        }
    }
    if names.is_empty() {
        notes.push(
            "No pci.ids on this system (the hwdata or pciutils package has it): PCI devices \
             are named by their numbers."
                .into(),
        );
    }
    devices.sort_by(|a, b| {
        (a.class.to_lowercase(), a.name.to_lowercase())
            .cmp(&(b.class.to_lowercase(), b.name.to_lowercase()))
    });
    Ok(Devices {
        devices,
        notes,
        took: started.elapsed(),
    })
}

fn read_text(p: &Path) -> Option<String> {
    let s = fs::read_to_string(p).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// The driver bound to a device: its name, and its module's version.
fn driver(dev: &Path) -> Option<Driver> {
    let link = fs::read_link(dev.join("driver")).ok()?;
    let name = link.file_name()?.to_string_lossy().to_string();
    let module = fs::read_link(dev.join("driver/module"))
        .ok()
        .and_then(|m| m.file_name().map(|n| n.to_string_lossy().to_string()));
    let version = module
        .as_deref()
        .and_then(|m| read_text(&Path::new("/sys/module").join(m).join("version")));
    Some(Driver {
        provider: None,
        version,
        date: None,
        inf: module,
        service: Some(name),
    })
}

/// pci.ids: vendor names by id, and device names by (vendor, device).
type PciNames = HashMap<(u16, Option<u16>), String>;

fn pci_names() -> PciNames {
    let mut out = HashMap::new();
    let text = [
        "/usr/share/hwdata/pci.ids",
        "/usr/share/misc/pci.ids",
        "/usr/share/pci.ids",
    ]
    .iter()
    .find_map(|p| fs::read_to_string(p).ok())
    .unwrap_or_default();
    parse_pci_ids(&text, &mut out);
    out
}

fn parse_pci_ids(text: &str, out: &mut PciNames) {
    let mut vendor: Option<u16> = None;
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        // The device classes come last, under "C xx".
        if line.starts_with("C ") {
            break;
        }
        if let Some(rest) = line.strip_prefix('\t') {
            if rest.starts_with('\t') {
                continue;
            }
            if let (Some(v), Some((id, name))) = (vendor, rest.split_once("  "))
                && let Ok(id) = u16::from_str_radix(id.trim(), 16)
            {
                out.insert((v, Some(id)), name.trim().to_string());
            }
        } else if let Some((id, name)) = line.split_once("  ") {
            vendor = u16::from_str_radix(id.trim(), 16).ok();
            if let Some(v) = vendor {
                out.insert((v, None), name.trim().to_string());
            }
        }
    }
}

fn hex(p: &Path) -> Option<u32> {
    let s = read_text(p)?;
    u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()
}

/// A PCI class code's kind, and whether such a device needs a driver.
fn pci_class(class: u32) -> (&'static str, bool) {
    match class >> 16 {
        0x01 => ("Storage", true),
        0x02 => ("Network", true),
        0x03 => ("Display", true),
        0x04 => ("Sound and video", true),
        0x05 => ("Memory", false),
        0x06 => ("Bridge", false),
        0x07 => ("Communication", false),
        0x08 => ("System", false),
        0x09 => ("Input", true),
        0x0C if (class >> 8) & 0xFF == 0x03 => ("USB", true),
        0x0C => ("Serial bus", false),
        0x0D => ("Wireless", true),
        0x10 => ("Encryption", false),
        0x11 => ("Signal processing", false),
        0x12 => ("Processing accelerator", true),
        _ => ("Other", false),
    }
}

fn pci(dev: &Path, names: &PciNames) -> Option<Device> {
    let vendor = hex(&dev.join("vendor"))? as u16;
    let device = hex(&dev.join("device"))? as u16;
    let class = hex(&dev.join("class")).unwrap_or(0);
    let (kind, needs) = pci_class(class);
    let driver = driver(dev);
    let maker = names.get(&(vendor, None)).cloned();
    let name = names
        .get(&(vendor, Some(device)))
        .cloned()
        .unwrap_or_else(|| format!("PCI device {vendor:04x}:{device:04x}"));
    let slot = dev.file_name()?.to_string_lossy().to_string();
    Some(Device {
        id: dev.display().to_string(),
        name,
        class: kind.into(),
        manufacturer: maker,
        location: Some(format!("PCI {slot}")),
        state: if driver.is_none() && needs {
            DeviceState::NoDriver
        } else {
            DeviceState::Working
        },
        driver,
        ..Device::default()
    })
}

fn usb(dev: &Path) -> Option<Device> {
    // Devices have a vendor id; their interfaces (1-1:1.0) do not.
    let vendor = read_text(&dev.join("idVendor"))?;
    let product = read_text(&dev.join("idProduct")).unwrap_or_default();
    let name =
        read_text(&dev.join("product")).unwrap_or_else(|| format!("USB device {vendor}:{product}"));
    // A device's driver is "usb"; its interfaces' drivers say what it is.
    let driver = fs::read_dir(dev)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(':'))
        .find_map(|e| driver(&e.path()))
        .or_else(|| driver(dev));
    Some(Device {
        id: dev.display().to_string(),
        name,
        class: "USB".into(),
        manufacturer: read_text(&dev.join("manufacturer")),
        location: dev
            .file_name()
            .map(|n| format!("USB {}", n.to_string_lossy())),
        state: DeviceState::Working,
        driver,
        ..Device::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pci_ids() {
        let mut names = HashMap::new();
        parse_pci_ids(
            "# comment\n10de  NVIDIA Corporation\n\t2684  AD102 [GeForce RTX 4090]\n\
             \t\t1043 889d  some subsystem\n1022  Advanced Micro Devices, Inc. [AMD]\n\
             C 00  Unclassified device\n\t00  Non-VGA\n",
            &mut names,
        );
        assert_eq!(names[&(0x10de, None)], "NVIDIA Corporation");
        assert_eq!(names[&(0x10de, Some(0x2684))], "AD102 [GeForce RTX 4090]");
        assert!(names.contains_key(&(0x1022, None)));
        assert_eq!(names.len(), 3);
        assert_eq!(pci_class(0x0c0330), ("USB", true));
        assert!(!pci_class(0x060400).1);
    }
}
