//! Physical disks: throughput, active time and latency from the partition
//! manager's counters (IOCTL_DISK_PERFORMANCE), plus identity, partitions
//! and volumes. None of the frequent queries sends I/O to the device, so a
//! sleeping disk stays asleep; the temperature query does, so it is
//! SSD-only and infrequent. A disk's partition table is read again only
//! when its volumes change, a volume without a letter is asked about once,
//! and neither happens while the disk sleeps.

use core::ffi::c_void;
use core::mem::{offset_of, size_of};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FindFirstVolumeW,
    FindNextVolumeW, FindVolumeClose, GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW,
    GetVolumePathNamesForVolumeNameW, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING,
};
use windows::Win32::System::Diagnostics::Debug::{
    SEM_FAILCRITICALERRORS, SetThreadErrorMode, THREAD_ERROR_MODE,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    DEVICE_SEEK_PENALTY_DESCRIPTOR, DISK_EXTENT, DISK_GEOMETRY_EX, DISK_PERFORMANCE,
    DRIVE_LAYOUT_INFORMATION_EX, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
    IOCTL_DISK_PERFORMANCE, IOCTL_STORAGE_QUERY_PROPERTY, PARTITION_INFORMATION_EX,
    PARTITION_STYLE_GPT, PARTITION_STYLE_MBR, PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR,
    STORAGE_PROPERTY_ID, STORAGE_PROPERTY_QUERY, STORAGE_TEMPERATURE_DATA_DESCRIPTOR,
    StorageDeviceProperty, StorageDeviceSeekPenaltyProperty, StorageDeviceTemperatureProperty,
    VOLUME_DISK_EXTENTS,
};
use windows::Win32::System::Power::GetDevicePowerState;
use windows::core::{BOOL, PCWSTR};

use crate::util::{OwnedHandle, from_wide, wide};

use super::*;

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;

struct Disk {
    handle: OwnedHandle,
    stats: DiskStats,
    prev: Option<DISK_PERFORMANCE>,
    /// The partition table as last read, before volumes are placed on it.
    layout: Vec<Partition>,
    /// Where its volumes started and how long they were when the table
    /// was read: when that changes, partitions were made, removed, grown
    /// or shrunk.
    places: Option<Vec<(u64, u64)>>,
}

pub struct DiskSampler {
    disks: Vec<Disk>,
    volumes_at: Option<Instant>,
    temperature_at: Option<Instant>,
    /// What the volumes without a letter or folder said, by volume name:
    /// they are asked once (see `volume`).
    unmounted: HashMap<String, Facts>,
}

/// Whether the disk spins, or does not sleep at all: asked without waking
/// it. Unknown counts as awake.
fn awake(handle: &OwnedHandle) -> bool {
    let mut on = BOOL(1);
    !unsafe { GetDevicePowerState(handle.raw(), &mut on) }.as_bool() || on.as_bool()
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

/// The drive's temperature, and where it warns and where it is in danger
/// (each None when the drive reports none).
fn temperature(handle: &OwnedHandle) -> Option<(i16, Option<i16>, Option<i16>)> {
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
    let mark = |m: i16| (m > 0 && m < 150).then_some(m);
    (d.InfoCount > 0 && t > -50 && t < 150).then_some((
        t,
        mark(d.WarningTemperature),
        mark(d.CriticalTemperature),
    ))
}

/// What a volume's file system says about it.
#[derive(Clone, Default)]
struct Facts {
    label: String,
    file_system: String,
    total: u64,
    free: u64,
}

/// A local volume, with or without a drive letter.
struct Mounted {
    /// (disk number, byte offset, length) of each piece: one unless it
    /// spans disks.
    extents: Vec<(u32, u64, u64)>,
    /// Where it is reachable: "C:\", "D:\Games\".
    paths: Vec<String>,
    facts: Facts,
}

/// Every local volume Windows knows. `unmounted` keeps what the volumes
/// without a path said; `asleep` tells a disk that is spun down.
fn mounted(unmounted: &mut HashMap<String, Facts>, asleep: &dyn Fn(u32) -> bool) -> Vec<Mounted> {
    // A card reader without a card has to fail quietly, not ask for one.
    let mut old = THREAD_ERROR_MODE(0);
    let quiet = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut old)) }.is_ok();
    let mut out = Vec::new();
    // "\\?\Volume{GUID}\" is 49 characters.
    let mut name = [0u16; 64];
    if let Ok(find) = unsafe { FindFirstVolumeW(&mut name) } {
        loop {
            out.extend(volume(&from_wide(&name), unmounted, asleep));
            if unsafe { FindNextVolumeW(find, &mut name) }.is_err() {
                break;
            }
        }
        let _ = unsafe { FindVolumeClose(find) };
    }
    if quiet {
        let _ = unsafe { SetThreadErrorMode(old, None) };
    }
    out
}

/// Label, file system and space, from the volume's file system.
fn facts(root: &[u16]) -> Facts {
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
    let mut file_system = [0u16; 32];
    let _ = unsafe {
        GetVolumeInformationW(
            PCWSTR(root.as_ptr()),
            Some(&mut label),
            None,
            None,
            None,
            Some(&mut file_system),
        )
    };
    Facts {
        label: from_wide(&label),
        file_system: from_wide(&file_system),
        total,
        free,
    }
}

/// One volume by its name, "\\?\Volume{GUID}\". Where it sits and where
/// it is reachable cost no I/O. Its facts do for a volume without a path
/// (an EFI or recovery partition, another system's): the first question
/// mounts it, reading the disk, and every file system Windows has tries an
/// unknown one. Those are asked once, and not while their disk sleeps.
fn volume(
    name: &str,
    unmounted: &mut HashMap<String, Facts>,
    asleep: &dyn Fn(u32) -> bool,
) -> Option<Mounted> {
    let root = wide(name);
    // Only disks: optical drives spin up to answer.
    let kind = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
    if kind != DRIVE_FIXED && kind != DRIVE_REMOVABLE {
        return None;
    }
    // The device is the name without its trailing backslash.
    let handle = open(name.trim_end_matches('\\'))?;
    let mut buf = [0u64; 32];
    if !ioctl(
        &handle,
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
        None,
        &mut buf,
    ) {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), size_of_val(&buf)) };
    let count = unsafe { buf.as_ptr().cast::<VOLUME_DISK_EXTENTS>().read_unaligned() }
        .NumberOfDiskExtents as usize;
    let first = offset_of!(VOLUME_DISK_EXTENTS, Extents);
    let step = size_of::<DISK_EXTENT>();
    let extents: Vec<(u32, u64, u64)> = (0..count)
        .map_while(|i| bytes.get(first + i * step..first + (i + 1) * step))
        .map(|raw| {
            let e = unsafe { raw.as_ptr().cast::<DISK_EXTENT>().read_unaligned() };
            (e.DiskNumber, e.StartingOffset as u64, e.ExtentLength as u64)
        })
        .collect();
    if extents.is_empty() {
        return None;
    }

    let mut list = [0u16; 1024];
    let mut length = 0u32;
    let paths = if unsafe {
        GetVolumePathNamesForVolumeNameW(PCWSTR(root.as_ptr()), Some(&mut list), &mut length)
    }
    .is_ok()
    {
        list.split(|&c| c == 0)
            .take_while(|p| !p.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    } else {
        Vec::new()
    };
    let facts = if !paths.is_empty() {
        facts(&root)
    } else if let Some(known) = unmounted.get(name) {
        known.clone()
    } else if extents.iter().any(|e| asleep(e.0)) {
        Facts::default()
    } else {
        let read = facts(&root);
        unmounted.insert(name.to_string(), read.clone());
        read
    };
    Some(Mounted {
        extents,
        paths,
        facts,
    })
}

/// Drive letters per physical disk number.
fn volumes(mounted: &[Mounted]) -> Vec<(u32, Volume)> {
    let mut out: Vec<(u32, Volume)> = mounted
        .iter()
        .flat_map(|m| {
            m.paths
                .iter()
                .filter(|p| p.len() == 3 && p.ends_with(":\\"))
                .map(|p| {
                    (
                        m.extents[0].0,
                        Volume {
                            letter: p[..2].to_string(),
                            label: m.facts.label.clone(),
                            total: m.facts.total,
                            free: m.facts.free,
                        },
                    )
                })
        })
        .collect();
    out.sort_by(|a, b| a.1.letter.cmp(&b.1.letter));
    out
}

/// The disk's partition table, in the order the partitions sit.
fn layout(handle: &OwnedHandle) -> (PartitionStyle, Vec<Partition>) {
    // Room for 128 entries, what GPT makes room for; an MBR disk with more
    // logical partitions than that gets none shown.
    let size = offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry)
        + 128 * size_of::<PARTITION_INFORMATION_EX>();
    let mut buf = vec![0u64; size.div_ceil(8)];
    let mut returned = 0u32;
    let read = unsafe {
        DeviceIoControl(
            handle.raw(),
            IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
            None,
            0,
            Some(buf.as_mut_ptr().cast()),
            (buf.len() * 8) as u32,
            Some(&mut returned),
            None,
        )
    }
    .is_ok();
    if !read {
        return (PartitionStyle::None, Vec::new());
    }
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len() * 8) };
    parse_layout(&bytes[..(returned as usize).min(bytes.len())])
}

/// A DRIVE_LAYOUT_INFORMATION_EX as the driver returned it.
fn parse_layout(bytes: &[u8]) -> (PartitionStyle, Vec<Partition>) {
    let word = |at: usize| {
        bytes
            .get(at..at + 4)
            .map_or(u32::MAX, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let style = match word(0) as i32 {
        s if s == PARTITION_STYLE_GPT.0 => PartitionStyle::Gpt,
        s if s == PARTITION_STYLE_MBR.0 => PartitionStyle::Mbr,
        _ => return (PartitionStyle::None, Vec::new()),
    };
    let first = offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry);
    let step = size_of::<PARTITION_INFORMATION_EX>();
    let mut out = Vec::new();
    for i in 0..word(4) as usize {
        let Some(raw) = bytes.get(first + i * step..first + (i + 1) * step) else {
            break;
        };
        let e = unsafe {
            raw.as_ptr()
                .cast::<PARTITION_INFORMATION_EX>()
                .read_unaligned()
        };
        if e.PartitionLength <= 0 {
            continue;
        }
        let (kind, role, name) = if e.PartitionStyle == PARTITION_STYLE_GPT {
            let gpt = unsafe { e.Anonymous.Gpt };
            let guid = gpt.PartitionType.to_u128();
            if guid == 0 {
                continue;
            }
            let (kind, role) = gpt_kind(guid);
            (kind, role, from_wide(&gpt.Name))
        } else if e.PartitionStyle == PARTITION_STYLE_MBR {
            let mbr = unsafe { e.Anonymous.Mbr };
            if mbr.PartitionType == 0 {
                continue;
            }
            let (kind, role) = mbr_kind(mbr.PartitionType);
            (kind, role, String::new())
        } else {
            continue;
        };
        // The extended partition only holds the logical ones, listed too.
        if role == PartitionRole::Extended {
            continue;
        }
        out.push(Partition {
            number: e.PartitionNumber,
            offset: e.StartingOffset as u64,
            size: e.PartitionLength as u64,
            kind,
            role,
            name,
            ..Default::default()
        });
    }
    out.sort_by_key(|p| p.offset);
    (style, out)
}

/// Fills in what Windows mounted from each of a disk's partitions.
fn place(partitions: &mut [Partition], disk: u32, mounted: &[Mounted], system: &str) {
    for m in mounted {
        for &(_, offset, _) in m.extents.iter().filter(|e| e.0 == disk) {
            let Some(p) = partitions
                .iter_mut()
                .find(|p| offset >= p.offset && offset < p.offset + p.size)
            else {
                continue;
            };
            // "C:\" reads as "C:", a folder without its last backslash.
            p.mounts = m
                .paths
                .iter()
                .map(|path| path.trim_end_matches('\\').to_string())
                .collect();
            p.system = p
                .mounts
                .iter()
                .any(|path| path.eq_ignore_ascii_case(system));
            p.label = m.facts.label.clone();
            p.file_system = m.facts.file_system.clone();
            p.total = m.facts.total;
            p.free = m.facts.free;
        }
    }
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
                    layout: Vec::new(),
                    places: None,
                })
            })
            .collect();
        Self {
            disks,
            volumes_at: None,
            temperature_at: None,
            unmounted: HashMap::new(),
        }
    }

    pub fn sample(&mut self) -> Vec<DiskStats> {
        let now = Instant::now();
        if self
            .volumes_at
            .is_none_or(|t| now - t > Duration::from_secs(30))
        {
            self.volumes_at = Some(now);
            let disks = &self.disks;
            let asleep = |number: u32| {
                disks
                    .iter()
                    .any(|d| d.stats.number == number && !awake(&d.handle))
            };
            let mounted = mounted(&mut self.unmounted, &asleep);
            let vols = volumes(&mounted);
            let system = std::env::var("SystemDrive").unwrap_or("C:".into());
            for disk in &mut self.disks {
                disk.stats.volumes = vols
                    .iter()
                    .filter(|(n, _)| *n == disk.stats.number)
                    .map(|(_, v)| v.clone())
                    .collect();
                // Read again only where its volumes changed, and once it is
                // awake: a sleeping disk is not woken for its table.
                let mut places: Vec<(u64, u64)> = mounted
                    .iter()
                    .flat_map(|m| &m.extents)
                    .filter(|e| e.0 == disk.stats.number)
                    .map(|e| (e.1, e.2))
                    .collect();
                places.sort_unstable();
                if disk.places.as_ref() != Some(&places) && awake(&disk.handle) {
                    (disk.stats.style, disk.layout) = layout(&disk.handle);
                    disk.places = Some(places);
                }
                disk.stats.partitions = disk.layout.clone();
                place(
                    &mut disk.stats.partitions,
                    disk.stats.number,
                    &mounted,
                    &system,
                );
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
                let read = temperature(&disk.handle);
                disk.stats.temperature = read.map(|r| r.0);
                (
                    disk.stats.temperature_warning,
                    disk.stats.temperature_critical,
                ) = read.map_or((None, None), |r| (r.1, r.2));
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
                "{} {} {} ssd={:?} {:.0} GB temp={:?} {:?} {:?}",
                d.number,
                d.model,
                d.bus,
                d.ssd,
                d.size as f64 / 1e9,
                d.temperature,
                d.volumes.iter().map(|v| &v.letter).collect::<Vec<_>>(),
                d.style
            );
            for p in &d.partitions {
                println!(
                    "  {:>2} {:>14} {:>14}  {:<22} {:?} name={:?} label={:?} fs={:?} \
                     mounts={:?} total={} free={} system={}",
                    p.number,
                    p.offset,
                    p.size,
                    p.kind,
                    p.role,
                    p.name,
                    p.label,
                    p.file_system,
                    p.mounts,
                    p.total,
                    p.free,
                    p.system
                );
            }
        }
        let windows: Vec<&Partition> = disks2
            .iter()
            .flat_map(|d| &d.partitions)
            .filter(|p| p.system)
            .collect();
        assert_eq!(windows.len(), 1, "{windows:?}");
        assert!(windows[0].total > 0 && !windows[0].file_system.is_empty());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    fn entry(number: u32, offset: i64, length: i64, gpt_type: u128) -> PARTITION_INFORMATION_EX {
        let mut e = PARTITION_INFORMATION_EX {
            PartitionStyle: PARTITION_STYLE_GPT,
            StartingOffset: offset,
            PartitionLength: length,
            PartitionNumber: number,
            ..Default::default()
        };
        let mut gpt = unsafe { e.Anonymous.Gpt };
        gpt.PartitionType = windows::core::GUID::from_u128(gpt_type);
        for (to, from) in gpt
            .Name
            .iter_mut()
            .zip("Basic data partition".encode_utf16())
        {
            *to = from;
        }
        e.Anonymous.Gpt = gpt;
        e
    }

    #[test]
    fn reads_a_drive_layout() {
        let first = offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry);
        let step = size_of::<PARTITION_INFORMATION_EX>();
        let entries = [
            entry(
                2,
                300 << 20,
                200 << 30,
                0xebd0a0a2_b9e5_4433_87c0_68b6b72699c7,
            ),
            entry(
                1,
                1 << 20,
                260 << 20,
                0xc12a7328_f81f_11d2_ba4b_00a0c93ec93b,
            ),
            // Unused slots come back zeroed.
            PARTITION_INFORMATION_EX {
                PartitionStyle: PARTITION_STYLE_GPT,
                ..Default::default()
            },
        ];
        let mut bytes = vec![0u8; first + entries.len() * step];
        bytes[..4].copy_from_slice(&(PARTITION_STYLE_GPT.0 as u32).to_le_bytes());
        // One more than there is room for: it must be ignored, not read.
        bytes[4..8].copy_from_slice(&(entries.len() as u32 + 1).to_le_bytes());
        for (i, e) in entries.iter().enumerate() {
            unsafe {
                bytes
                    .as_mut_ptr()
                    .add(first + i * step)
                    .cast::<PARTITION_INFORMATION_EX>()
                    .write_unaligned(*e)
            };
        }
        let (style, mut partitions) = parse_layout(&bytes);
        assert_eq!(style, PartitionStyle::Gpt);
        assert_eq!(partitions.len(), 2);
        assert_eq!(partitions[0].kind, "EFI system");
        assert_eq!(partitions[1].role, PartitionRole::Data);
        assert_eq!(partitions[1].name, "Basic data partition");

        let c = Mounted {
            extents: vec![(0, (300 << 20) as u64, 100)],
            paths: vec!["C:\\".into(), "D:\\Mount\\".into()],
            facts: Facts {
                label: "Windows".into(),
                file_system: "NTFS".into(),
                total: 100,
                free: 40,
            },
        };
        place(&mut partitions, 0, &[c], "c:");
        assert_eq!(partitions[1].mounts, ["C:", "D:\\Mount"]);
        assert!(partitions[1].system && !partitions[0].system);
        assert_eq!(partitions[1].file_system, "NTFS");
        assert!(parse_layout(&bytes[..6]).1.is_empty());
    }
}
