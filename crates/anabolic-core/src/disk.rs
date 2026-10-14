//! Physical disks: throughput, active time and latency from the partition
//! manager's counters (IOCTL_DISK_PERFORMANCE), plus identity and volumes.
//! None of these queries sends I/O to the device, so a sleeping disk stays
//! asleep; the temperature query does, so it is SSD-only and infrequent.

use core::ffi::c_void;
use core::mem::size_of;
use std::time::{Duration, Instant};

use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW,
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
    OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    DEVICE_SEEK_PENALTY_DESCRIPTOR, DISK_GEOMETRY_EX, DISK_PERFORMANCE,
    IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_PERFORMANCE, IOCTL_STORAGE_QUERY_PROPERTY,
    PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_ID, STORAGE_PROPERTY_QUERY,
    STORAGE_TEMPERATURE_DATA_DESCRIPTOR, StorageDeviceProperty, StorageDeviceSeekPenaltyProperty,
    StorageDeviceTemperatureProperty, VOLUME_DISK_EXTENTS,
};
use windows::core::PCWSTR;

use crate::util::{OwnedHandle, from_wide, wide};

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;

#[derive(Clone, Debug, Default)]
pub struct Volume {
    /// "C:"
    pub letter: String,
    pub label: String,
    pub total: u64,
    pub free: u64,
}

#[derive(Clone, Debug, Default)]
pub struct DiskStats {
    pub number: u32,
    pub model: String,
    pub bus: &'static str,
    pub ssd: Option<bool>,
    pub size: u64,
    pub volumes: Vec<Volume>,
    pub read_rate: f64,
    pub write_rate: f64,
    pub read_iops: f64,
    pub write_iops: f64,
    /// Share of the interval the disk had I/O outstanding, 0..=100.
    pub active: f32,
    pub response_ms: f32,
    pub queue: u32,
    pub temperature: Option<i16>,
}

struct Disk {
    handle: OwnedHandle,
    stats: DiskStats,
    prev: Option<DISK_PERFORMANCE>,
}

pub struct DiskSampler {
    disks: Vec<Disk>,
    volumes_at: Option<Instant>,
    temperature_at: Option<Instant>,
}

fn ioctl<T>(handle: &OwnedHandle, code: u32, input: Option<&[u8]>, out: &mut T) -> bool {
    let mut returned = 0u32;
    unsafe {
        DeviceIoControl(
            handle.raw(),
            code,
            input.map(|i| i.as_ptr().cast::<c_void>()),
            input.map_or(0, |i| i.len() as u32),
            Some((out as *mut T).cast()),
            size_of::<T>() as u32,
            Some(&mut returned),
            None,
        )
    }
    .is_ok()
}

fn open(path: &str) -> Option<OwnedHandle> {
    let path = wide(path);
    // No access rights needed for these queries; never blocks on the medium.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            0,
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

fn property_query(id: STORAGE_PROPERTY_ID) -> Vec<u8> {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: id,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    unsafe {
        std::slice::from_raw_parts(
            (&raw const query).cast::<u8>(),
            size_of::<STORAGE_PROPERTY_QUERY>(),
        )
    }
    .to_vec()
}

fn ascii_at(buf: &[u8], offset: u32) -> String {
    let start = offset as usize;
    if start == 0 || start >= buf.len() {
        return String::new();
    }
    let end = buf[start..]
        .iter()
        .position(|&b| b == 0)
        .map_or(buf.len(), |n| start + n);
    String::from_utf8_lossy(&buf[start..end]).trim().to_string()
}

fn bus_name(bus: i32) -> &'static str {
    match bus {
        1 => "SCSI",
        2 => "ATAPI",
        3 => "ATA",
        4 => "1394",
        6 => "Fibre Channel",
        7 => "USB",
        8 => "RAID",
        9 => "iSCSI",
        10 => "SAS",
        11 => "SATA",
        12 => "SD",
        13 => "MMC",
        14 | 15 => "Virtual",
        16 => "Storage Spaces",
        17 => "NVMe",
        18 => "SCM",
        19 => "UFS",
        _ => "",
    }
}

fn identify(handle: &OwnedHandle, number: u32) -> DiskStats {
    let mut stats = DiskStats {
        number,
        ..Default::default()
    };
    let mut buf = [0u64; 128];
    if ioctl(
        handle,
        IOCTL_STORAGE_QUERY_PROPERTY,
        Some(&property_query(StorageDeviceProperty)),
        &mut buf,
    ) {
        let bytes =
            unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), size_of_val(&buf)) };
        let d = unsafe {
            buf.as_ptr()
                .cast::<STORAGE_DEVICE_DESCRIPTOR>()
                .read_unaligned()
        };
        let vendor = ascii_at(bytes, d.VendorIdOffset);
        let product = ascii_at(bytes, d.ProductIdOffset);
        stats.model = if vendor.is_empty() || product.starts_with(&vendor) {
            product
        } else {
            format!("{vendor} {product}")
        };
        stats.bus = bus_name(d.BusType.0);
    }
    let mut penalty = DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
    if ioctl(
        handle,
        IOCTL_STORAGE_QUERY_PROPERTY,
        Some(&property_query(StorageDeviceSeekPenaltyProperty)),
        &mut penalty,
    ) {
        stats.ssd = Some(!penalty.IncursSeekPenalty);
    }
    let mut geometry = [0u64; 16];
    if ioctl(
        handle,
        IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
        None,
        &mut geometry,
    ) {
        stats.size = unsafe {
            geometry
                .as_ptr()
                .cast::<DISK_GEOMETRY_EX>()
                .read_unaligned()
        }
        .DiskSize as u64;
    }
    if stats.model.is_empty() {
        stats.model = format!("Disk {number}");
    }
    stats
}

fn temperature(handle: &OwnedHandle) -> Option<i16> {
    let mut buf = [0u64; 64];
    if !ioctl(
        handle,
        IOCTL_STORAGE_QUERY_PROPERTY,
        Some(&property_query(StorageDeviceTemperatureProperty)),
        &mut buf,
    ) {
        return None;
    }
    let d = unsafe {
        buf.as_ptr()
            .cast::<STORAGE_TEMPERATURE_DATA_DESCRIPTOR>()
            .read_unaligned()
    };
    let t = d.TemperatureInfo[0].Temperature;
    (d.InfoCount > 0 && t > -50 && t < 150).then_some(t)
}

/// Drive letters per physical disk number.
fn volumes() -> Vec<(u32, Volume)> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u8 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = format!("{}:", (b'A' + i) as char);
        let root = wide(&format!("{letter}\\"));
        // Only local disks: network drives can hang for a long time.
        let kind = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
        if kind != DRIVE_FIXED && kind != DRIVE_REMOVABLE {
            continue;
        }
        let Some(handle) = open(&format!(r"\\.\{letter}")) else {
            continue;
        };
        let mut extents = [0u64; 32];
        if !ioctl(
            &handle,
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            None,
            &mut extents,
        ) {
            continue;
        }
        let header = unsafe {
            extents
                .as_ptr()
                .cast::<VOLUME_DISK_EXTENTS>()
                .read_unaligned()
        };
        if header.NumberOfDiskExtents == 0 {
            continue;
        }
        let (mut total, mut free) = (0u64, 0u64);
        let _ = unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(root.as_ptr()),
                None,
                Some(&mut total),
                Some(&mut free),
            )
        };
        let mut label = [0u16; 64];
        let _ = unsafe {
            GetVolumeInformationW(
                PCWSTR(root.as_ptr()),
                Some(&mut label),
                None,
                None,
                None,
                None,
            )
        };
        out.push((
            header.Extents[0].DiskNumber,
            Volume {
                letter,
                label: from_wide(&label),
                total,
                free,
            },
        ));
    }
    out
}

impl DiskSampler {
    pub fn new() -> Self {
        let disks = (0..32u32)
            .filter_map(|n| {
                let handle = open(&format!(r"\\.\PhysicalDrive{n}"))?;
                let stats = identify(&handle, n);
                Some(Disk {
                    handle,
                    stats,
                    prev: None,
                })
            })
            .collect();
        Self {
            disks,
            volumes_at: None,
            temperature_at: None,
        }
    }

    pub fn sample(&mut self) -> Vec<DiskStats> {
        let now = Instant::now();
        if self
            .volumes_at
            .is_none_or(|t| now - t > Duration::from_secs(30))
        {
            self.volumes_at = Some(now);
            let vols = volumes();
            for disk in &mut self.disks {
                disk.stats.volumes = vols
                    .iter()
                    .filter(|(n, _)| *n == disk.stats.number)
                    .map(|(_, v)| v.clone())
                    .collect();
            }
        }
        let read_temperature = self
            .temperature_at
            .is_none_or(|t| now - t > Duration::from_secs(15));
        if read_temperature {
            self.temperature_at = Some(now);
        }

        for disk in &mut self.disks {
            if read_temperature && disk.stats.ssd == Some(true) {
                disk.stats.temperature = temperature(&disk.handle);
            }
            let mut perf = DISK_PERFORMANCE::default();
            if !ioctl(&disk.handle, IOCTL_DISK_PERFORMANCE, None, &mut perf) {
                continue;
            }
            let s = &mut disk.stats;
            s.queue = perf.QueueDepth;
            if let Some(prev) = disk.prev {
                let span = (perf.QueryTime - prev.QueryTime).max(1) as f64; // 100 ns
                let secs = span / 1e7;
                let reads = perf.ReadCount.wrapping_sub(prev.ReadCount) as f64;
                let writes = perf.WriteCount.wrapping_sub(prev.WriteCount) as f64;
                s.read_rate = (perf.BytesRead - prev.BytesRead).max(0) as f64 / secs;
                s.write_rate = (perf.BytesWritten - prev.BytesWritten).max(0) as f64 / secs;
                s.read_iops = reads / secs;
                s.write_iops = writes / secs;
                let idle = (perf.IdleTime - prev.IdleTime).max(0) as f64;
                s.active = ((1.0 - idle / span) * 100.0).clamp(0.0, 100.0) as f32;
                let busy_time = ((perf.ReadTime - prev.ReadTime)
                    + (perf.WriteTime - prev.WriteTime))
                    .max(0) as f64;
                s.response_ms = if reads + writes > 0.0 {
                    (busy_time / (reads + writes) / 1e4) as f32
                } else {
                    0.0
                };
            }
            disk.prev = Some(perf);
        }
        self.disks.iter().map(|d| d.stats.clone()).collect()
    }
}

impl Default for DiskSampler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_system_disk() {
        let mut sampler = DiskSampler::new();
        let started = Instant::now();
        let disks = sampler.sample();
        std::thread::sleep(Duration::from_millis(200));
        let disks2 = sampler.sample();
        assert!(!disks.is_empty(), "no physical disk found");
        let system = std::env::var("SystemDrive")
            .unwrap_or("C:".into())
            .to_uppercase();
        assert!(
            disks2
                .iter()
                .any(|d| d.volumes.iter().any(|v| v.letter == system)),
            "{disks2:?}"
        );
        assert!(disks2.iter().all(|d| (0.0..=100.0).contains(&d.active)));
        for d in &disks2 {
            println!(
                "{} {} {} ssd={:?} {:.0} GB temp={:?} {:?}",
                d.number,
                d.model,
                d.bus,
                d.ssd,
                d.size as f64 / 1e9,
                d.temperature,
                d.volumes.iter().map(|v| &v.letter).collect::<Vec<_>>()
            );
        }
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
