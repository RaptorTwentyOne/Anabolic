// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Interrupts on Windows. What the hardware can do comes from the PCI
//! bus driver's properties (InterruptSupport, InterruptMessageMaximum);
//! how a device interrupts now from the interrupts it was given (a message
//! shows as a number with the top bit set, as Device Manager's negative
//! IRQs); Windows' settings from the device's own registry key, under
//! Device Parameters\Interrupt Management, where the MSI utilities write
//! too: MessageSignaledInterruptProperties\MSISupported, and Affinity
//! Policy's DevicePolicy with AssignmentSetOverride for the processors.

use windows::Win32::Devices::DeviceAndDriverInstallation::ResType_IRQ;
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_CompatibleIds, DEVPKEY_Device_DeviceDesc, DEVPKEY_Device_FriendlyName,
};
use windows::Win32::Foundation::DEVPROPKEY;
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
use windows::core::GUID;

use crate::devices::{instance_ids, list, node, number, resources, text};
use crate::interrupts::{Change, Device, Kind};
use crate::reg;

const PCI: GUID = GUID::from_u128(0x3ab22e31_8264_4b4e_9af5_a8d2d8e33e62);
/// PCI_INTERRUPT_SUPPORT_*: 1 line-based, 2 MSI, 4 MSI-X.
const INTERRUPT_SUPPORT: DEVPROPKEY = DEVPROPKEY {
    fmtid: PCI,
    pid: 14,
};
const MESSAGE_MAXIMUM: DEVPROPKEY = DEVPROPKEY {
    fmtid: PCI,
    pid: 15,
};
/// IrqPolicySpecifiedProcessors.
const SPECIFIED: u64 = 4;

fn management(id: &str) -> String {
    format!(r"SYSTEM\CurrentControlSet\Enum\{id}\Device Parameters\Interrupt Management")
}

/// The PCI class code ("0300") from a device's compatible ids.
fn class_code(compatible: &[String]) -> Option<String> {
    compatible.iter().find_map(|c| {
        let up = c.to_uppercase();
        let at = up.find("CC_")? + 3;
        up.get(at..at + 4).map(str::to_string)
    })
}

fn device(id: &str) -> Option<Device> {
    let inst = node(id)?;
    let compatible = list(inst, &DEVPKEY_Device_CompatibleIds);
    let class = class_code(&compatible).unwrap_or_default();
    // Bridges route others' interrupts.
    if class.starts_with("06") {
        return None;
    }
    // IRQ_DES_64: count, type, flags, then the number it was given.
    let given: Vec<u32> = resources(inst, ResType_IRQ)
        .iter()
        .filter_map(|d| Some(u32::from_le_bytes(d.get(12..16)?.try_into().ok()?)))
        .collect();
    if given.is_empty() {
        return None;
    }
    let messages = given.iter().filter(|&&n| n >= 0x8000_0000).count() as u32;
    let support = number(inst, &INTERRUPT_SUPPORT).unwrap_or(0);
    let base = management(id);
    let msi_setting = reg::number_at(
        HKEY_LOCAL_MACHINE,
        &format!(r"{base}\MessageSignaledInterruptProperties"),
        "MSISupported",
    )
    .map(|n| n != 0);
    let policy = format!(r"{base}\Affinity Policy");
    let cores = (reg::number_at(HKEY_LOCAL_MACHINE, &policy, "DevicePolicy") == Some(SPECIFIED))
        .then(|| reg::number_at(HKEY_LOCAL_MACHINE, &policy, "AssignmentSetOverride"))
        .flatten()
        .filter(|&m| m != 0);
    Some(Device {
        id: id.to_string(),
        name: text(inst, &DEVPKEY_Device_FriendlyName)
            .or_else(|| text(inst, &DEVPKEY_Device_DeviceDesc))
            .unwrap_or_else(|| "Device".into()),
        kind: Kind::from_class(&class),
        line: support & 1 != 0,
        msi: support & 2 != 0,
        msix: support & 4 != 0,
        max_messages: number(inst, &MESSAGE_MAXIMUM).unwrap_or(0),
        messages_now: (messages > 0).then_some(messages),
        msi_setting,
        cores,
    })
}

/// The PCI devices that interrupt, by kind and name.
pub fn read() -> Result<Vec<Device>, String> {
    let mut out: Vec<Device> = instance_ids(Some("PCI"))?
        .iter()
        .filter_map(|id| device(id))
        .collect();
    out.sort_by_key(|d| (d.kind, d.name.to_lowercase()));
    Ok(out)
}

/// Changes a device's setting; it takes effect when the device next
/// starts. Says what was done.
pub fn set(id: &str, change: Change) -> Result<String, String> {
    let d = device(id).ok_or("That device is not connected, or does not interrupt.")?;
    let base = management(id);
    match change {
        Change::Messages(want) => {
            if !d.changeable() {
                return Err(if d.kind == Kind::Storage {
                    format!(
                        "{} controls drives: if its driver cannot start the new way, Windows \
cannot start. Anabolic leaves it as it is.",
                        d.name
                    )
                } else {
                    format!("{} cannot interrupt by message.", d.name)
                });
            }
            let key = format!(r"{base}\MessageSignaledInterruptProperties");
            reg::set_number(HKEY_LOCAL_MACHINE, &key, "MSISupported", want as u32)?;
        }
        Change::Cores(mask) => {
            let key = format!(r"{base}\Affinity Policy");
            match mask {
                Some(m) if m != 0 => {
                    reg::set_number(HKEY_LOCAL_MACHINE, &key, "DevicePolicy", SPECIFIED as u32)?;
                    reg::set_bytes(
                        HKEY_LOCAL_MACHINE,
                        &key,
                        "AssignmentSetOverride",
                        &m.to_le_bytes(),
                    )?;
                }
                _ => {
                    reg::delete_value(HKEY_LOCAL_MACHINE, &key, "DevicePolicy")?;
                    reg::delete_value(HKEY_LOCAL_MACHINE, &key, "AssignmentSetOverride")?;
                }
            }
        }
    }
    Ok(format!(
        "{}: {}. It takes effect at the next restart.",
        d.name,
        change.words().to_lowercase()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_class_code() {
        let ids = vec![
            r"PCI\VEN_10DE&DEV_2C02&REV_A1".to_string(),
            r"PCI\VEN_10DE&CC_030000".to_string(),
            r"PCI\CC_0300".to_string(),
        ];
        assert_eq!(class_code(&ids).as_deref(), Some("0300"));
    }

    #[test]
    fn leaves_drive_controllers_alone() {
        // Refused before anything is written.
        if let Some(d) = read()
            .unwrap()
            .into_iter()
            .find(|d| d.kind == Kind::Storage)
        {
            let e = set(&d.id, Change::Messages(false)).unwrap_err();
            assert!(e.contains("Windows cannot start"), "{e}");
        }
        assert!(set(r"PCI\VEN_FFFF&DEV_FFFF\0", Change::Cores(None)).is_err());
    }

    #[test]
    fn reads_this_machines_interrupts() {
        let devices = read().unwrap();
        assert!(!devices.is_empty());
        for d in &devices {
            println!(
                "{:<9} {:<50} now: {:<18} can: {:<36} set: {:?} cores: {:?}",
                d.kind.label(),
                d.name,
                d.now_words(),
                d.can_words(),
                d.msi_setting,
                d.cores.map(crate::interrupts::cores_words)
            );
        }
    }
}
