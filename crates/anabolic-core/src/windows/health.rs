// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Drive health on Windows. An NVMe drive's health log through the storage
//! stack's protocol query; a SATA drive's SMART attributes and thresholds
//! through the disk driver's SMART requests (administrator), or its
//! attributes alone through the failure prediction query. Each drive is
//! asked only while it is awake.

use core::ffi::c_void;
use core::mem::size_of;

use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR,
    STORAGE_PROPERTY_QUERY, StorageDeviceProperty,
};
use windows::Win32::System::Power::GetDevicePowerState;
use windows::core::{BOOL, PCWSTR};

use crate::health::{Health, attributes_of, from_nvme_log};
use crate::util::{OwnedHandle, wide};

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
/// CTL_CODE(IOCTL_STORAGE_BASE, 0x0440, METHOD_BUFFERED, FILE_ANY_ACCESS).
const IOCTL_STORAGE_PREDICT_FAILURE: u32 = 0x002D_1100;
/// CTL_CODE(IOCTL_DISK_BASE, 0x0022, METHOD_BUFFERED, read and write).
const SMART_RCV_DRIVE_DATA: u32 = 0x0007_C088;
const STORAGE_DEVICE_PROTOCOL_SPECIFIC_PROPERTY: u32 = 50;
const PROTOCOL_TYPE_NVME: u32 = 3;
const NVME_DATA_TYPE_LOG_PAGE: u32 = 2;
const NVME_LOG_HEALTH: u32 = 2;
const BUS_NVME: i32 = 17;
const BUS_SATA: i32 = 11;
const BUS_ATA: i32 = 3;

fn open(n: u32, access: u32) -> Option<OwnedHandle> {
    let path = wide(&format!(r"\\.\PhysicalDrive{n}"));
    let handle = unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .ok()?;
    Some(OwnedHandle(handle))
}

/// A request with `input` in and up to `out.len()` bytes back; the bytes
/// returned.
fn ask(handle: &OwnedHandle, code: u32, input: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut got = 0u32;
    unsafe {
        DeviceIoControl(
            handle.raw(),
            code,
            Some(input.as_ptr().cast::<c_void>()),
            input.len() as u32,
            Some(out.as_mut_ptr().cast::<c_void>()),
            out.len() as u32,
            Some(&mut got),
            None,
        )
    }
    .ok()?;
    Some(got as usize)
}

fn asleep(handle: &OwnedHandle) -> bool {
    let mut on = BOOL(1);
    unsafe { GetDevicePowerState(handle.raw(), &mut on) }.as_bool() && !on.as_bool()
}

/// The model and the bus it is on.
fn describe(handle: &OwnedHandle) -> (String, i32) {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let input = unsafe {
        std::slice::from_raw_parts(
            (&raw const query).cast::<u8>(),
            size_of::<STORAGE_PROPERTY_QUERY>(),
        )
    };
    let mut buf = vec![0u8; 1024];
    if ask(handle, IOCTL_STORAGE_QUERY_PROPERTY, input, &mut buf).is_none() {
        return (String::new(), 0);
    }
    let d = unsafe {
        buf.as_ptr()
            .cast::<STORAGE_DEVICE_DESCRIPTOR>()
            .read_unaligned()
    };
    let text = |offset: u32| {
        let start = offset as usize;
        if start == 0 || start >= buf.len() {
            return String::new();
        }
        let end = buf[start..]
            .iter()
            .position(|&b| b == 0)
            .map_or(buf.len(), |n| start + n);
        String::from_utf8_lossy(&buf[start..end]).trim().to_string()
    };
    let (vendor, product) = (text(d.VendorIdOffset), text(d.ProductIdOffset));
    let model = if vendor.is_empty() || product.starts_with(&vendor) {
        product
    } else {
        format!("{vendor} {product}")
    };
    (model, d.BusType.0)
}

/// The NVMe health log page.
fn nvme_log(handle: &OwnedHandle) -> Option<Vec<u8>> {
    // STORAGE_PROPERTY_QUERY's id and type, then the protocol request
    // (STORAGE_PROTOCOL_SPECIFIC_DATA, 40 bytes), then room for the page.
    let mut buf = vec![0u8; 8 + 40 + 512];
    let put = |buf: &mut [u8], at: usize, v: u32| buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut buf, 0, STORAGE_DEVICE_PROTOCOL_SPECIFIC_PROPERTY);
    put(&mut buf, 4, PropertyStandardQuery.0 as u32);
    put(&mut buf, 8, PROTOCOL_TYPE_NVME);
    put(&mut buf, 12, NVME_DATA_TYPE_LOG_PAGE);
    put(&mut buf, 16, NVME_LOG_HEALTH);
    put(&mut buf, 24, 40);
    put(&mut buf, 28, 512);
    let input = buf.clone();
    ask(handle, IOCTL_STORAGE_QUERY_PROPERTY, &input, &mut buf)?;
    // STORAGE_PROTOCOL_DATA_DESCRIPTOR: version, size, then the request as
    // answered, whose offset counts from its own start.
    let offset = u32::from_le_bytes(buf[8 + 16..8 + 20].try_into().ok()?) as usize;
    let length = u32::from_le_bytes(buf[8 + 20..8 + 24].try_into().ok()?) as usize;
    buf.get(8 + offset..8 + offset + length.min(512))
        .filter(|d| d.len() >= 200)
        .map(<[u8]>::to_vec)
}

/// A SMART read (attributes 0xD0, thresholds 0xD1) through the disk
/// driver: 512 bytes.
fn smart_read(handle: &OwnedHandle, feature: u8) -> Option<Vec<u8>> {
    // SENDCMDINPARAMS: buffer size, the IDE registers, the drive number.
    let mut input = vec![0u8; 32];
    input[0..4].copy_from_slice(&512u32.to_le_bytes());
    input[4..12].copy_from_slice(&[feature, 1, 1, 0x4F, 0xC2, 0xA0, 0xB0, 0]);
    // SENDCMDOUTPARAMS: size, the driver's status (12 bytes), the data.
    let mut out = vec![0u8; 16 + 512];
    let got = ask(handle, SMART_RCV_DRIVE_DATA, &input, &mut out)?;
    (got >= 16 + 362 && out[4] == 0).then(|| out[16..].to_vec())
}

/// The drive's own failure prediction, with its SMART data for an ATA
/// drive.
fn predict(handle: &OwnedHandle) -> Option<(bool, Vec<u8>)> {
    let mut out = vec![0u8; 4 + 512];
    ask(handle, IOCTL_STORAGE_PREDICT_FAILURE, &[], &mut out)?;
    let fails = u32::from_le_bytes(out[0..4].try_into().ok()?) != 0;
    Some((fails, out[4..].to_vec()))
}

pub fn read() -> Vec<Health> {
    let mut all = Vec::new();
    for n in 0..32u32 {
        let Some(handle) = open(n, 0) else {
            continue;
        };
        let (model, bus) = describe(&handle);
        let mut h = Health {
            number: n,
            model: if model.is_empty() {
                format!("Disk {n}")
            } else {
                model
            },
            protocol: match bus {
                BUS_NVME => "NVMe",
                BUS_SATA | BUS_ATA => "SATA",
                _ => "",
            },
            ..Health::default()
        };
        if asleep(&handle) {
            h.note = Some("It sleeps: not woken to ask".into());
            all.push(h);
            continue;
        }
        // Some requests need the drive opened for reading and writing,
        // which only an administrator may.
        let full = open(n, GENERIC_READ | GENERIC_WRITE);
        if bus == BUS_NVME {
            let log = nvme_log(&handle).or_else(|| full.as_ref().and_then(nvme_log));
            match log {
                Some(log) => from_nvme_log(&mut h, &log),
                None => h.note = Some("The drive did not hand over its health log".into()),
            }
        } else {
            let predicted = predict(&handle).or_else(|| full.as_ref().and_then(predict));
            let data = full.as_ref().and_then(|f| smart_read(f, 0xD0));
            let thresholds = full.as_ref().and_then(|f| smart_read(f, 0xD1));
            let attributes = data
                .as_deref()
                .or(predicted.as_ref().map(|(_, d)| d.as_slice()))
                .map(|d| attributes_of(d, thresholds.as_deref()))
                .unwrap_or_default();
            if let Some((fails, _)) = &predicted {
                h.drive_warns = *fails;
            }
            if attributes.is_empty() {
                h.note = Some(if full.is_none() {
                    "Its SMART data needs administrator rights".into()
                } else {
                    "The drive does not report SMART data (a USB bridge often hides it)".into()
                });
            } else {
                h.take_attributes(attributes);
            }
        }
        h.judge();
        all.push(h);
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prints this machine's drives as read. Needs an administrator for
    /// SATA thresholds.
    #[test]
    #[ignore]
    fn reads_this_machines_drives() {
        for h in read() {
            println!(
                "{} {} [{}] {:?} {:?}: wear {:?} temp {:?} hours {:?} cycles {:?} unsafe {:?} written {:?} media {:?} realloc {:?} pending {:?} spare {:?} warns {} attrs {} note {:?}",
                h.number,
                h.model,
                h.protocol,
                h.verdict,
                h.reasons,
                h.wear_used,
                h.temperature,
                h.power_on_hours,
                h.power_cycles,
                h.unsafe_shutdowns,
                h.written,
                h.media_errors,
                h.reallocated,
                h.pending,
                h.spare,
                h.drive_warns,
                h.attributes.len(),
                h.note
            );
        }
    }
}
