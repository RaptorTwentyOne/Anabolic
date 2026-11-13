// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Devices from the Configuration Manager, as Device Manager reads them:
//! every device instance Windows knows, connected or not (CM_Get_Device_ID_
//! List), each one's state and problem (CM_Get_DevNode_Status) and its
//! properties (CM_Get_DevNode_Property: name, class, maker, location, the
//! driver's provider, version, date, INF and service, when it was first
//! installed and last connected or removed). Nothing is opened.

use std::collections::HashMap;
use std::time::Instant;

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_CLASS_PROPERTY_INSTALLER, CM_DEVNODE_STATUS_FLAGS, CM_GETIDLIST_FILTER_ENUMERATOR, CM_GETIDLIST_FILTER_NONE, CM_GETIDLIST_FILTER_PRESENT,
    CM_Get_Class_PropertyW, CM_Get_DevNode_PropertyW, CM_Get_DevNode_Status,
    CM_Get_Device_ID_List_SizeW, CM_Get_Device_ID_ListW, CM_LOCATE_DEVNODE_PHANTOM,
    CM_Locate_DevNodeW, CM_PROB, CR_SUCCESS, DN_HAS_PROBLEM, DN_NO_SHOW_IN_DM,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_Class, DEVPKEY_Device_ClassGuid, DEVPKEY_Device_DeviceDesc,
    DEVPKEY_Device_DriverDate, DEVPKEY_Device_DriverInfPath, DEVPKEY_Device_DriverProvider,
    DEVPKEY_Device_DriverVersion, DEVPKEY_Device_FirstInstallDate, DEVPKEY_Device_FriendlyName,
    DEVPKEY_Device_LastArrivalDate, DEVPKEY_Device_LastRemovalDate, DEVPKEY_Device_LocationInfo,
    DEVPKEY_Device_Manufacturer, DEVPKEY_Device_Parent, DEVPKEY_Device_ProblemCode,
    DEVPKEY_Device_ProblemStatus, DEVPKEY_Device_Service, DEVPKEY_DeviceClass_Name,
    DEVPROP_TYPE_FILETIME, DEVPROP_TYPE_GUID, DEVPROP_TYPE_NTSTATUS, DEVPROP_TYPE_STRING,
    DEVPROP_TYPE_STRING_LIST, DEVPROP_TYPE_UINT32, DEVPROPTYPE,
};
use windows::Win32::Foundation::DEVPROPKEY;
use windows::core::{GUID, PCWSTR};

use crate::devices::{Device, DeviceState, Devices, Driver};
use crate::util::wide;

/// No problem number: CM_PROB_NOT_CONFIGURED (1) and the rest start at 1.
const CM_PROB_DISABLED: u32 = 22;
const CM_PROB_FAILED_INSTALL: u32 = 28;

pub fn read() -> Result<Devices, String> {
    let started = Instant::now();
    let ids = instance_ids(None)?;
    let mut classes = HashMap::new();
    let mut devices: Vec<Device> = ids
        .iter()
        .filter_map(|id| device(id, &mut classes))
        .collect();
    devices.sort_by(|a, b| {
        (a.class.to_lowercase(), a.name.to_lowercase())
            .cmp(&(b.class.to_lowercase(), b.name.to_lowercase()))
    });
    Ok(Devices {
        devices,
        notes: Vec::new(),
        took: started.elapsed(),
    })
}

/// Every device instance Windows knows of, connected or not; or with an
/// enumerator ("PCI", "USB"), those of that bus that are connected now.
pub(crate) fn instance_ids(enumerator: Option<&str>) -> Result<Vec<String>, String> {
    let name = enumerator.map(wide);
    let (filter, flags) = match &name {
        Some(n) => (
            PCWSTR(n.as_ptr()),
            CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT,
        ),
        None => (PCWSTR::null(), CM_GETIDLIST_FILTER_NONE),
    };
    // The list can grow between the two calls: a few tries.
    for _ in 0..3 {
        let mut len = 0u32;
        let cr = unsafe { CM_Get_Device_ID_List_SizeW(&mut len, filter, flags) };
        if cr != CR_SUCCESS {
            return Err(format!(
                "the device list could not be read (error {})",
                cr.0
            ));
        }
        let mut buf = vec![0u16; len as usize + 1];
        let cr = unsafe { CM_Get_Device_ID_ListW(filter, &mut buf, flags) };
        if cr == CR_SUCCESS {
            return Ok(buf
                .split(|&c| c == 0)
                .filter(|s| !s.is_empty())
                .map(String::from_utf16_lossy)
                .collect());
        }
    }
    Err("the device list kept changing while it was read".into())
}

/// A connected device's node, by its instance id.
pub(crate) fn node(id: &str) -> Option<u32> {
    let wid = wide(id);
    let mut inst = 0u32;
    let cr = unsafe { CM_Locate_DevNodeW(&mut inst, PCWSTR(wid.as_ptr()), Default::default()) };
    (cr == CR_SUCCESS).then_some(inst)
}

/// A property's raw bytes and type.
pub(crate) fn property(inst: u32, key: &DEVPROPKEY) -> Option<(DEVPROPTYPE, Vec<u8>)> {
    let mut kind = DEVPROPTYPE(0);
    let mut size = 0u32;
    // The first call says how much room it needs.
    let _ = unsafe { CM_Get_DevNode_PropertyW(inst, key, &mut kind, None, &mut size, 0) };
    if size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let cr = unsafe {
        CM_Get_DevNode_PropertyW(inst, key, &mut kind, Some(buf.as_mut_ptr()), &mut size, 0)
    };
    (cr == CR_SUCCESS).then(|| {
        buf.truncate(size as usize);
        (kind, buf)
    })
}

pub(crate) fn text(inst: u32, key: &DEVPROPKEY) -> Option<String> {
    let (kind, bytes) = property(inst, key)?;
    if kind != DEVPROP_TYPE_STRING {
        return None;
    }
    utf16(&bytes)
}

pub(crate) fn number(inst: u32, key: &DEVPROPKEY) -> Option<u32> {
    let (kind, bytes) = property(inst, key)?;
    if kind != DEVPROP_TYPE_UINT32 && kind != DEVPROP_TYPE_NTSTATUS {
        return None;
    }
    Some(u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?))
}

/// A list of strings (hardware and compatible ids).
pub(crate) fn list(inst: u32, key: &DEVPROPKEY) -> Vec<String> {
    match property(inst, key) {
        Some((kind, bytes)) if kind == DEVPROP_TYPE_STRING_LIST => bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes(*b))
            .collect::<Vec<u16>>()
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect(),
        _ => Vec::new(),
    }
}

fn filetime(inst: u32, key: &DEVPROPKEY) -> Option<i64> {
    let (kind, bytes) = property(inst, key)?;
    if kind != DEVPROP_TYPE_FILETIME {
        return None;
    }
    let t = i64::from_le_bytes(bytes.get(..8)?.try_into().ok()?);
    (t > 0).then_some(t)
}

/// A string in a property's bytes (UTF-16, ending in a nul).
fn utf16(bytes: &[u8]) -> Option<String> {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .take_while(|&c| c != 0)
        .collect();
    let s = String::from_utf16_lossy(&units).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// A device class's name as Device Manager shows it ("Disk drives"),
/// by the class's GUID.
fn class_name(guid: &GUID) -> Option<String> {
    let mut kind = DEVPROPTYPE(0);
    let mut buf = [0u8; 512];
    let mut size = buf.len() as u32;
    let cr = unsafe {
        CM_Get_Class_PropertyW(
            guid,
            &DEVPKEY_DeviceClass_Name,
            &mut kind,
            Some(buf.as_mut_ptr()),
            &mut size,
            CM_CLASS_PROPERTY_INSTALLER,
        )
    };
    (cr == CR_SUCCESS && kind == DEVPROP_TYPE_STRING)
        .then(|| utf16(&buf[..size as usize]))
        .flatten()
}

/// The device's class, named as Device Manager names it when it can be.
fn class_of(inst: u32, classes: &mut HashMap<GUID, Option<String>>) -> Option<String> {
    let guid = property(inst, &DEVPKEY_Device_ClassGuid)
        .filter(|(kind, bytes)| *kind == DEVPROP_TYPE_GUID && bytes.len() >= 16)
        .map(|(_, bytes)| GUID::from_u128(u128::from_be_bytes(guid_bytes(&bytes))));
    let named = guid.and_then(|g| classes.entry(g).or_insert_with(|| class_name(&g)).clone());
    named.or_else(|| text(inst, &DEVPKEY_Device_Class))
}

/// A GUID's 16 bytes as Windows lays them out (three little-endian
/// numbers, then eight bytes), in the order GUID::from_u128 reads.
fn guid_bytes(b: &[u8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&[b[3], b[2], b[1], b[0]]);
    out[4..6].copy_from_slice(&[b[5], b[4]]);
    out[6..8].copy_from_slice(&[b[7], b[6]]);
    out[8..16].copy_from_slice(&b[8..16]);
    out
}

fn device(id: &str, classes: &mut HashMap<GUID, Option<String>>) -> Option<Device> {
    let wid = wide(id);
    let mut inst = 0u32;
    let cr =
        unsafe { CM_Locate_DevNodeW(&mut inst, PCWSTR(wid.as_ptr()), CM_LOCATE_DEVNODE_PHANTOM) };
    if cr != CR_SUCCESS {
        return None;
    }
    let mut status = CM_DEVNODE_STATUS_FLAGS(0);
    let mut problem = CM_PROB(0);
    // A device that is not connected has no status: it is a phantom.
    let present =
        unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, inst, 0) } == CR_SUCCESS;
    let problem = if present {
        (status.0 & DN_HAS_PROBLEM.0 != 0).then_some(problem.0)
    } else {
        number(inst, &DEVPKEY_Device_ProblemCode).filter(|&p| p != 0)
    };
    let service = text(inst, &DEVPKEY_Device_Service);
    let inf = text(inst, &DEVPKEY_Device_DriverInfPath);
    let state = if !present {
        DeviceState::NotConnected
    } else {
        match problem {
            Some(CM_PROB_DISABLED) => DeviceState::Disabled,
            Some(CM_PROB_FAILED_INSTALL) => DeviceState::NoDriver,
            Some(p) => DeviceState::Problem(p),
            None => DeviceState::Working,
        }
    };
    let driver = Driver {
        provider: text(inst, &DEVPKEY_Device_DriverProvider),
        version: text(inst, &DEVPKEY_Device_DriverVersion),
        date: filetime(inst, &DEVPKEY_Device_DriverDate),
        inf,
        service,
    };
    let name = text(inst, &DEVPKEY_Device_FriendlyName)
        .or_else(|| text(inst, &DEVPKEY_Device_DeviceDesc))
        .unwrap_or_else(|| "Unknown device".into());
    Some(Device {
        id: id.to_string(),
        name,
        class: class_of(inst, classes).unwrap_or_else(|| "Other devices".into()),
        manufacturer: text(inst, &DEVPKEY_Device_Manufacturer),
        location: text(inst, &DEVPKEY_Device_LocationInfo),
        state,
        hidden: present && status.0 & DN_NO_SHOW_IN_DM.0 != 0,
        status_code: number(inst, &DEVPKEY_Device_ProblemStatus).filter(|&s| s != 0),
        driver: (driver != Driver::default()).then_some(driver),
        installed: filetime(inst, &DEVPKEY_Device_FirstInstallDate),
        arrived: filetime(inst, &DEVPKEY_Device_LastArrivalDate),
        removed: filetime(inst, &DEVPKEY_Device_LastRemovalDate),
        parent: text(inst, &DEVPKEY_Device_Parent),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_this_machines_devices() {
        let d = read().unwrap();
        assert!(d.devices.len() > 20, "{} devices", d.devices.len());
        // Every machine has a processor among its devices, and it works;
        // Device Manager calls their class "Processors".
        assert!(
            d.devices
                .iter()
                .any(|x| x.class == "Processors" && x.state == DeviceState::Working)
        );
        for x in crate::devices::troubled(&d.devices) {
            println!("{:?} {} [{}] {:?}", x.state, x.name, x.class, x.status_code);
        }
        let (present, gone) = d
            .devices
            .iter()
            .partition::<Vec<&Device>, _>(|x| x.state != DeviceState::NotConnected);
        println!(
            "{} devices ({} connected, {} not) in {:?}",
            d.devices.len(),
            present.len(),
            gone.len(),
            d.took
        );
    }
}
