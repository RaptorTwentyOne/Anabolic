// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What Windows lets a program read without a driver of its own: the
//! firmware's thermal zones (from the performance counters) and the
//! processor's energy meters. Neither needs admin rights.
//!
//! Windows gives the processor's own temperature sensors only to kernel
//! drivers, and Anabolic installs none. The thermal zones are what the
//! firmware (ACPI) watches to cool the machine: on a laptop usually near
//! the processor, on many desktops a fixed placeholder, on others none at
//! all. The energy meters are the processor's RAPL counters, which AMD's
//! and Intel's power drivers publish through the Energy Meter Interface:
//! from them comes the power the package draws.

use std::sync::Arc;

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW,
    CM_Get_Device_Interface_ListW, CR_SUCCESS,
};
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Performance::{
    PDH_FMT, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW,
};
use windows::Win32::System::Power::{
    GUID_DEVICE_ENERGY_METER, IOCTL_EMI_GET_MEASUREMENT, IOCTL_EMI_GET_METADATA,
    IOCTL_EMI_GET_METADATA_SIZE, IOCTL_EMI_GET_VERSION,
};
use windows::core::{PCWSTR, w};

use crate::util::{OwnedHandle, wide};

use super::*;

/// The value as the counter stores it, without its display scale.
const PDH_FMT_NOSCALE: u32 = 0x1000;

/// The device thermal zones are listed under.
pub const ZONES: &str = "Thermal zones";

/// "\_TZ.THRM" as "THRM": the zone's own name in the firmware.
fn zone_label(instance: &str) -> String {
    let name = instance.rsplit('.').next().unwrap_or(instance);
    name.trim_matches('\\').to_string()
}

/// A zone's reading in °C: tenths of a kelvin (or whole kelvins, from the
/// older counter); None for a zone that reports nothing real.
fn celsius(value: f64, tenths: bool) -> Option<f32> {
    let kelvin = if tenths { value / 10.0 } else { value };
    let c = (kelvin - 273.15) as f32;
    (-40.0..=150.0).contains(&c).then_some(c)
}

struct Query {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    /// The counter is in tenths of a kelvin.
    tenths: bool,
}

impl Drop for Query {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

impl Query {
    fn open() -> Option<Self> {
        let mut query = PDH_HQUERY(std::ptr::null_mut());
        if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) } != 0 {
            return None;
        }
        let mut out = Self {
            query,
            counter: PDH_HCOUNTER::default(),
            tenths: true,
        };
        // The precise counter came with Windows 10; the older one is in
        // whole kelvins.
        for (path, tenths) in [
            (
                w!(r"\Thermal Zone Information(*)\High Precision Temperature"),
                true,
            ),
            (w!(r"\Thermal Zone Information(*)\Temperature"), false),
        ] {
            let mut counter = PDH_HCOUNTER::default();
            if unsafe { PdhAddEnglishCounterW(out.query, path, 0, &mut counter) } == 0 {
                out.counter = counter;
                out.tenths = tenths;
                return Some(out);
            }
        }
        None
    }

    fn read(&self) -> Vec<Sensor> {
        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            return Vec::new();
        }
        let format = PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOSCALE);
        let (mut size, mut count) = (0u32, 0u32);
        let status = unsafe {
            PdhGetFormattedCounterArrayW(self.counter, format, &mut size, &mut count, None)
        };
        if status != PDH_MORE_DATA || size == 0 {
            return Vec::new();
        }
        // The names live in the same buffer, after the items.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                self.counter,
                format,
                &mut size,
                &mut count,
                Some(buffer.as_mut_ptr().cast()),
            )
        };
        let fits = count as usize * size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>() <= buffer.len() * 8;
        if status != 0 || !fits {
            return Vec::new();
        }
        let items = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(),
                count as usize,
            )
        };
        let device: Arc<str> = Arc::from(ZONES);
        let mut out: Vec<Sensor> = items
            .iter()
            .filter(|item| item.FmtValue.CStatus <= 1 && !item.szName.is_null())
            .filter_map(|item| {
                let value = celsius(unsafe { item.FmtValue.Anonymous.doubleValue }, self.tenths)?;
                let name = unsafe { item.szName.to_string() }.ok()?;
                Some(Sensor {
                    device: device.clone(),
                    label: Arc::from(zone_label(&name)),
                    kind: SensorKind::Temperature,
                    value,
                    high: None,
                    critical: None,
                })
            })
            .collect();
        out.sort_by(|a, b| a.label.cmp(&b.label));
        out
    }
}

/// Which of a meter's channels is shown, and as what. RAPL channels are
/// named like "RAPL_Package0_PKG": the package; on Intel also its cores
/// (PP0), graphics (PP1), memory (DRAM) and the whole platform (PSYS).
/// AMD's per-core channels are left out: the package says it in one row.
fn rapl_label(channel: &str) -> Option<String> {
    let name = channel.to_ascii_lowercase();
    if !name.starts_with("rapl") {
        return None;
    }
    let what = match name.rsplit('_').next()? {
        "pkg" => "Package",
        "pp0" => "Cores",
        "pp1" => "Graphics",
        "dram" => "Memory",
        "psys" => "Platform",
        _ => return None,
    };
    // A second processor's package: "Package 1".
    let package: u32 = name
        .split('_')
        .find_map(|part| part.strip_prefix("package")?.parse().ok())
        .unwrap_or(0);
    Some(match package {
        0 => format!("{what} power"),
        n => format!("{what} {n} power"),
    })
}

/// Watts from two readings of an energy counter: picowatt-hours over
/// 100-ns ticks.
fn watts(energy: u64, time: u64, before: (u64, u64)) -> Option<f32> {
    let (energy0, time0) = before;
    let ticks = time.checked_sub(time0).filter(|&t| t > 0)?;
    let joules = energy.checked_sub(energy0)? as f64 * 3.6e-9;
    Some((joules / (ticks as f64 * 1e-7)) as f32)
}

struct Channel {
    /// None: not shown.
    label: Option<Arc<str>>,
    /// The last (energy, time) read.
    last: Option<(u64, u64)>,
}

/// One energy meter device.
struct Meter {
    handle: OwnedHandle,
    channels: Vec<Channel>,
}

fn ioctl(handle: &OwnedHandle, code: u32, out: &mut [u8]) -> Option<usize> {
    let mut returned = 0u32;
    unsafe {
        DeviceIoControl(
            handle.raw(),
            code,
            None,
            0,
            Some(out.as_mut_ptr().cast()),
            out.len() as u32,
            Some(&mut returned),
            None,
        )
    }
    .ok()
    .map(|()| returned as usize)
}

/// The names of a meter's channels, from its metadata: one for a version 1
/// meter, any number for a version 2 one.
fn channel_names(handle: &OwnedHandle) -> Vec<String> {
    let mut version = [0u8; 2];
    let mut size = [0u8; 4];
    if ioctl(handle, IOCTL_EMI_GET_VERSION, &mut version).is_none()
        || ioctl(handle, IOCTL_EMI_GET_METADATA_SIZE, &mut size).is_none()
    {
        return Vec::new();
    }
    let mut meta = vec![0u8; u32::from_le_bytes(size).min(64 * 1024) as usize];
    let Some(len) = ioctl(handle, IOCTL_EMI_GET_METADATA, &mut meta) else {
        return Vec::new();
    };
    parse_channels(u16::from_le_bytes(version), &meta[..len.min(meta.len())])
}

/// EMI_METADATA_V1: unit, OEM and model (16 UTF-16 units each), revision,
/// then one name. EMI_METADATA_V2: OEM, model, revision, channel count,
/// then per channel its unit, the name's size in bytes and the name.
/// Channels that are not in picowatt-hours (unit 0) get no name.
fn parse_channels(version: u16, meta: &[u8]) -> Vec<String> {
    let u16_at = |at: usize| {
        meta.get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let i32_at = |at: usize| {
        meta.get(at..at + 4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let name_at = |at: usize, bytes: usize| -> Option<String> {
        let raw = meta.get(at..at + bytes)?;
        let units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .take_while(|&c| c != 0)
            .collect();
        Some(String::from_utf16_lossy(&units))
    };
    match version {
        1 => {
            let name = (i32_at(0) == Some(0))
                .then(|| name_at(72, u16_at(70)? as usize))
                .flatten();
            vec![name.unwrap_or_default()]
        }
        2 => {
            let count = u16_at(66).unwrap_or(0);
            let mut at = 68;
            let mut out = Vec::new();
            for _ in 0..count {
                let (Some(unit), Some(bytes)) = (i32_at(at), u16_at(at + 4)) else {
                    break;
                };
                let name = name_at(at + 6, bytes as usize);
                out.push(if unit == 0 {
                    name.unwrap_or_default()
                } else {
                    String::new()
                });
                at += 6 + bytes as usize;
            }
            out
        }
        _ => Vec::new(),
    }
}

/// The energy meters this PC has, with the channels worth showing.
fn meters() -> Vec<Meter> {
    let mut len = 0u32;
    let listed = unsafe {
        CM_Get_Device_Interface_List_SizeW(
            &mut len,
            &GUID_DEVICE_ENERGY_METER,
            PCWSTR::null(),
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        )
    };
    if listed != CR_SUCCESS || len <= 1 {
        return Vec::new();
    }
    let mut list = vec![0u16; len as usize];
    if unsafe {
        CM_Get_Device_Interface_ListW(
            &GUID_DEVICE_ENERGY_METER,
            PCWSTR::null(),
            &mut list,
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        )
    } != CR_SUCCESS
    {
        return Vec::new();
    }
    list.split(|&c| c == 0)
        .filter(|path| !path.is_empty())
        .filter_map(|path| {
            let path = wide(&String::from_utf16_lossy(path));
            let handle = unsafe {
                CreateFileW(
                    PCWSTR(path.as_ptr()),
                    GENERIC_READ.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
            }
            .ok()?;
            let handle = OwnedHandle(handle);
            let channels: Vec<Channel> = channel_names(&handle)
                .iter()
                .map(|name| Channel {
                    label: rapl_label(name).map(Arc::from),
                    last: None,
                })
                .collect();
            channels
                .iter()
                .any(|c| c.label.is_some())
                .then_some(Meter { handle, channels })
        })
        .collect()
}

impl Meter {
    fn read(&mut self, out: &mut Vec<Sensor>) {
        let mut data = vec![0u8; self.channels.len() * 16];
        let Some(len) = ioctl(&self.handle, IOCTL_EMI_GET_MEASUREMENT, &mut data) else {
            return;
        };
        let u64_at = |at: usize| {
            data.get(at..at + 8)
                .and_then(|b| b.try_into().ok())
                .map(u64::from_le_bytes)
        };
        for (i, channel) in self.channels.iter_mut().enumerate() {
            if i * 16 + 16 > len {
                break;
            }
            let (Some(energy), Some(time)) = (u64_at(i * 16), u64_at(i * 16 + 8)) else {
                break;
            };
            let before = channel.last.replace((energy, time));
            if let Some(label) = &channel.label
                && let Some(value) = before.and_then(|b| watts(energy, time, b))
            {
                out.push(Sensor {
                    device: Arc::from(CPU),
                    label: label.clone(),
                    kind: SensorKind::Power,
                    value,
                    high: None,
                    critical: None,
                });
            }
        }
    }
}

pub struct SensorSampler {
    /// None once it is known there are no thermal zones to read.
    zones: Option<Query>,
    zones_read: bool,
    meters: Vec<Meter>,
}

impl Default for SensorSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl SensorSampler {
    pub fn new() -> Self {
        Self {
            zones: Query::open(),
            zones_read: false,
            meters: meters(),
        }
    }

    pub fn sample(&mut self) -> Vec<Sensor> {
        let mut out = Vec::new();
        for meter in &mut self.meters {
            meter.read(&mut out);
        }
        if let Some(zones) = &self.zones {
            let read = zones.read();
            // Firmware has its zones from the start or never: none on
            // the first read, and the counters are not asked again.
            if read.is_empty() && !self.zones_read {
                self.zones = None;
            }
            self.zones_read = true;
            out.extend(read);
        }
        out
    }

    /// Nothing left to read: the thread can end.
    pub fn idle(&self) -> bool {
        self.zones.is_none() && self.meters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_units() {
        assert_eq!(zone_label(r"\_TZ.THRM"), "THRM");
        assert_eq!(zone_label(r"\_SB.PC00.LPCB.EC0.TZ01"), "TZ01");
        assert_eq!(zone_label("CPUZ"), "CPUZ");
        assert!((celsius(3232.0, true).unwrap() - 50.05).abs() < 0.01);
        assert_eq!(celsius(323.0, false).map(f32::round), Some(50.0));
        assert_eq!(celsius(0.0, true), None);
    }

    #[test]
    fn names_rapl_channels() {
        assert_eq!(
            rapl_label("RAPL_Package0_PKG").as_deref(),
            Some("Package power")
        );
        assert_eq!(
            rapl_label("RAPL_Package1_PKG").as_deref(),
            Some("Package 1 power")
        );
        assert_eq!(
            rapl_label("RAPL_Package0_PP0").as_deref(),
            Some("Cores power")
        );
        assert_eq!(
            rapl_label("RAPL_Package0_DRAM").as_deref(),
            Some("Memory power")
        );
        assert_eq!(rapl_label("RAPL_Package0_Core3_CORE"), None);
        assert_eq!(rapl_label("Battery"), None);
    }

    #[test]
    fn energy_over_time_is_power() {
        // 1 Wh is 1e12 pWh; over one hour of 100-ns ticks: 1 W.
        let hour = 36_000_000_000;
        assert!((watts(1_000_000_000_000, hour, (0, 0)).unwrap() - 1.0).abs() < 1e-6);
        assert_eq!(watts(5, 10, (5, 10)), None);
        assert_eq!(watts(4, 20, (5, 10)), None);
    }

    #[test]
    fn parses_metadata() {
        let name = |s: &str| -> Vec<u8> {
            s.encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect()
        };
        let mut v2 = vec![0u8; 66];
        v2.extend(2u16.to_le_bytes());
        for channel in ["RAPL_Package0_PKG", "RAPL_Package0_Core0_CORE"] {
            let n = name(channel);
            v2.extend(0i32.to_le_bytes());
            v2.extend((n.len() as u16).to_le_bytes());
            v2.extend(n);
        }
        assert_eq!(
            parse_channels(2, &v2),
            ["RAPL_Package0_PKG", "RAPL_Package0_Core0_CORE"]
        );
        let mut v1 = vec![0u8; 70];
        let n = name("RAPL_Package0_PKG");
        v1.extend((n.len() as u16).to_le_bytes());
        v1.extend(n);
        assert_eq!(parse_channels(1, &v1), ["RAPL_Package0_PKG"]);
    }

    #[test]
    fn reads_the_sensors() {
        let mut sampler = SensorSampler::new();
        sampler.sample();
        std::thread::sleep(std::time::Duration::from_millis(500));
        for s in sampler.sample() {
            println!(
                "{} / {}: {:.1} {}",
                s.device,
                s.label,
                s.value,
                s.kind.unit()
            );
            if s.kind == SensorKind::Temperature {
                assert!((-40.0..=150.0).contains(&s.value));
            }
        }
    }
}
