//! Physical disks from /sys/block and /proc/diskstats: what each one is,
//! how busy, its temperature, its partitions and the file systems mounted
//! from it.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use super::*;

/// The counters of one /proc/diskstats line that matter here.
#[derive(Clone, Copy)]
struct Counters {
    reads: u64,
    sectors_read: u64,
    ms_reading: u64,
    writes: u64,
    sectors_written: u64,
    ms_writing: u64,
    in_flight: u64,
    ms_active: u64,
    at: Instant,
}

struct Disk {
    /// The kernel's name: nvme0n1, sda.
    name: String,
    stats: DiskStats,
    prev: Option<Counters>,
}

pub struct DiskSampler {
    disks: Vec<Disk>,
    volumes_at: Option<Instant>,
    temperature_at: Option<Instant>,
}

impl Default for DiskSampler {
    fn default() -> Self {
        Self::new()
    }
}

fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// A whole disk, not a partition, a loop file, RAM or a mapping on top of
/// others (which would count their traffic twice).
fn physical(name: &str) -> bool {
    !["loop", "ram", "zram", "dm-", "md", "sr", "fd"]
        .iter()
        .any(|p| name.starts_with(p))
        && Path::new(&format!("/sys/block/{name}/device")).exists()
}

/// How the disk is attached.
fn bus(name: &str) -> &'static str {
    if name.starts_with("nvme") {
        return "NVMe";
    }
    if name.starts_with("mmcblk") {
        return "SD";
    }
    if name.starts_with("vd") || name.starts_with("xvd") {
        return "Virtual";
    }
    let device = std::fs::canonicalize(format!("/sys/block/{name}/device"))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    if device.contains("/usb") {
        "USB"
    } else if device.contains("/ata") {
        "SATA"
    } else {
        "SCSI"
    }
}

fn identify(name: &str, number: u32) -> DiskStats {
    let base = format!("/sys/block/{name}");
    let model = read(format!("{base}/device/model"))
        .or_else(|| read(format!("{base}/device/name")))
        .unwrap_or_else(|| name.to_string());
    let vendor = read(format!("{base}/device/vendor")).filter(|v| !v.is_empty() && v != "ATA");
    DiskStats {
        number,
        model: match vendor {
            Some(vendor) if !model.starts_with(&vendor) => format!("{vendor} {model}"),
            _ => model,
        },
        bus: bus(name),
        ssd: read(format!("{base}/queue/rotational")).map(|r| r == "0"),
        size: read(format!("{base}/size"))
            .and_then(|s| s.parse::<u64>().ok())
            .map_or(0, |sectors| sectors * 512),
        ..Default::default()
    }
}

/// The disk's temperature, from its hardware monitor (NVMe drives have
/// one; SATA ones with the drivetemp module).
fn temperature(name: &str) -> Option<i16> {
    let dir = std::fs::read_dir(format!("/sys/block/{name}/device/hwmon"))
        .or_else(|_| std::fs::read_dir(format!("/sys/block/{name}/device")))
        .ok()?;
    for entry in dir.flatten() {
        let file = entry.file_name();
        if !file.to_string_lossy().starts_with("hwmon") {
            continue;
        }
        if let Some(milli) =
            read(entry.path().join("temp1_input")).and_then(|t| t.parse::<i32>().ok())
        {
            return Some((milli / 1000) as i16);
        }
    }
    None
}

/// The whole disk a block device lives on: a partition's parent, the
/// disks under a mapping (LUKS, LVM).
fn disks_under(device: &str) -> Vec<String> {
    let Ok(real) = std::fs::canonicalize(format!("/sys/class/block/{device}")) else {
        return Vec::new();
    };
    if Path::new(&format!("/sys/block/{device}")).exists() {
        let slaves: Vec<String> = std::fs::read_dir(real.join("slaves"))
            .map(|d| {
                d.flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        if slaves.is_empty() {
            return vec![device.to_string()];
        }
        return slaves.iter().flat_map(|s| disks_under(s)).collect();
    }
    // A partition: its directory sits in the disk's.
    real.parent()
        .and_then(|p| p.file_name())
        .map(|n| vec![n.to_string_lossy().into_owned()])
        .unwrap_or_default()
}

/// File systems' labels by device name, from /dev/disk/by-label.
fn labels() -> HashMap<String, String> {
    let Ok(dir) = std::fs::read_dir("/dev/disk/by-label") else {
        return HashMap::new();
    };
    dir.flatten()
        .filter_map(|e| {
            let target = std::fs::canonicalize(e.path()).ok()?;
            let device = target.file_name()?.to_string_lossy().into_owned();
            let label = unescape(&e.file_name().to_string_lossy());
            Some((device, label))
        })
        .collect()
}

/// udev writes "\x20" for a space in a label.
fn unescape(text: &str) -> String {
    let mut out = Vec::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("\\x") {
        out.extend_from_slice(&rest.as_bytes()[..at]);
        match rest
            .get(at + 2..at + 4)
            .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            Some(b) => {
                out.push(b);
                rest = &rest[at + 4..];
            }
            None => {
                out.extend_from_slice(b"\\x");
                rest = &rest[at + 2..];
            }
        }
    }
    out.extend_from_slice(rest.as_bytes());
    // Escaped bytes can be parts of one UTF-8 character.
    String::from_utf8_lossy(&out).into_owned()
}

/// The mount table's escapes: "\040" for a space.
fn unescape_octal(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && let Some(code) = text
                .get(i + 1..i + 4)
                .and_then(|o| u8::from_str_radix(o, 8).ok())
        {
            out.push(code);
            i += 4;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A file system mounted from a block device.
struct Mount {
    /// The kernel's name for the device: nvme0n1p2, dm-0.
    device: String,
    point: String,
    fs_type: String,
}

/// Every mount of a block device, in the order they were made.
fn mounts() -> Vec<Mount> {
    let Ok(table) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in table.lines() {
        // id parent major:minor root mount-point options ... - type source super-options
        let fields: Vec<&str> = line.split(' ').collect();
        let Some(dash) = fields.iter().position(|f| *f == "-") else {
            continue;
        };
        let (Some(number), Some(point)) = (fields.get(2), fields.get(4)) else {
            continue;
        };
        // The block device by its numbers: the source's name can be a link
        // (/dev/root, /dev/disk/by-uuid/…) that is not there to follow.
        let Some(device) = std::fs::canonicalize(format!("/sys/dev/block/{number}"))
            .ok()
            .and_then(|p| Some(p.file_name()?.to_string_lossy().into_owned()))
        else {
            continue;
        };
        let point = unescape_octal(point);
        if seen.insert((device.clone(), point.clone())) {
            out.push(Mount {
                device,
                point,
                fs_type: fields.get(dash + 1).copied().unwrap_or("").to_string(),
            });
        }
    }
    out
}

/// Size and room left per mounted device, from its first mount: bind
/// mounts and subvolumes show the same file system again.
fn spaces(mounts: &[Mount]) -> HashMap<String, (u64, u64)> {
    let mut out = HashMap::new();
    for m in mounts {
        if !out.contains_key(&m.device) {
            out.insert(m.device.clone(), space(&m.point).unwrap_or((0, 0)));
        }
    }
    out
}

/// Every mounted file system on a disk: (disk name, volume).
fn volumes(
    mounts: &[Mount],
    spaces: &HashMap<String, (u64, u64)>,
    labels: &HashMap<String, String>,
) -> Vec<(String, Volume)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in mounts {
        // Once, where it was first mounted (usually the whole of it).
        if !seen.insert(&m.device) {
            continue;
        }
        let (total, free) = spaces.get(&m.device).copied().unwrap_or((0, 0));
        for disk in disks_under(&m.device) {
            out.push((
                disk,
                Volume {
                    letter: m.point.clone(),
                    label: labels
                        .get(&m.device)
                        .cloned()
                        .unwrap_or_else(|| m.fs_type.clone()),
                    total,
                    free,
                },
            ));
        }
    }
    out
}

/// Swap in use on block devices: device name -> (size, used).
fn swaps() -> HashMap<String, (u64, u64)> {
    let Ok(text) = std::fs::read_to_string("/proc/swaps") else {
        return HashMap::new();
    };
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_ascii_whitespace().collect();
            if f.get(1) != Some(&"partition") {
                return None;
            }
            let path = std::fs::canonicalize(unescape_octal(f.first()?)).ok()?;
            let kib = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024;
            Some((
                path.file_name()?.to_string_lossy().into_owned(),
                (kib(2), kib(3)),
            ))
        })
        .collect()
}

/// What udev knows of a block device ("259:2"): its partition table entry
/// and what blkid found in it.
fn udev(dev: &str) -> HashMap<String, String> {
    std::fs::read_to_string(format!("/run/udev/data/b{dev}"))
        .map(|text| {
            text.lines()
                .filter_map(|l| l.strip_prefix("E:")?.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// The devices built on top of one, all the way up: LUKS and LVM
/// mappings, RAID arrays.
fn holders(device: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Ok(dir) = std::fs::read_dir(format!("/sys/class/block/{device}/holders")) else {
        return out;
    };
    for name in dir
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
    {
        let above = holders(&name);
        for n in std::iter::once(name).chain(above) {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

/// Containers rather than file systems: what matters is inside.
const CONTAINERS: [&str; 3] = ["crypto_LUKS", "LVM2_member", "linux_raid_member"];

/// blkid's names, as people know them.
fn file_system_name(fs: &str, version: &str) -> String {
    match fs {
        "vfat" if version.starts_with("FAT") => version,
        "vfat" | "msdos" => "FAT",
        "ntfs" | "ntfs3" | "fuseblk" => "NTFS",
        "exfat" => "exFAT",
        "crypto_LUKS" => "LUKS",
        "LVM2_member" => "LVM",
        "linux_raid_member" => "RAID",
        "zfs_member" => "ZFS",
        "hfsplus" => "HFS+",
        "apfs" => "APFS",
        "iso9660" => "ISO 9660",
        other => other,
    }
    .to_string()
}

/// What the contents say over what the type claims: swap made in a
/// "Linux filesystem" partition is swap, and /boot boots.
fn role_from_contents(role: PartitionRole, fs: &str, mounts: &[String]) -> PartitionRole {
    match fs {
        "swap" => PartitionRole::Swap,
        "crypto_LUKS" | "BitLocker" => PartitionRole::Encrypted,
        "LVM2_member" | "linux_raid_member" => PartitionRole::Pool,
        _ if role == PartitionRole::Data
            && mounts
                .iter()
                .any(|m| ["/boot", "/boot/efi", "/efi"].contains(&m.as_str())) =>
        {
            PartitionRole::Boot
        }
        _ => role,
    }
}

/// A disk's partition table and what is in and on each partition.
fn partitions(
    disk: &str,
    mounts: &[Mount],
    spaces: &HashMap<String, (u64, u64)>,
    swaps: &HashMap<String, (u64, u64)>,
    labels: &HashMap<String, String>,
) -> (PartitionStyle, Vec<Partition>) {
    let style_of = |scheme: &str| match scheme {
        "gpt" => PartitionStyle::Gpt,
        "dos" => PartitionStyle::Mbr,
        _ => PartitionStyle::None,
    };
    let base = format!("/sys/block/{disk}");
    let mut style = read(format!("{base}/dev"))
        .map(|dev| style_of(udev(&dev).get("ID_PART_TABLE_TYPE").map_or("", |s| s)))
        .unwrap_or_default();
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir(&base) else {
        return (style, out);
    };
    for entry in dir.flatten() {
        let path = entry.path();
        let Some(number) = read(path.join("partition")).and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let device = entry.file_name().to_string_lossy().into_owned();
        // sysfs counts 512-byte sectors whatever the disk's own size.
        let bytes = |file: &str| {
            read(path.join(file))
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0)
                * 512
        };
        let facts = read(path.join("dev")).map(|d| udev(&d)).unwrap_or_default();
        let fact = |key: &str| facts.get(key).map_or("", |s| s.as_str());
        let scheme = fact("ID_PART_ENTRY_SCHEME");
        if style == PartitionStyle::None {
            style = style_of(scheme);
        }
        let entry_type = fact("ID_PART_ENTRY_TYPE");
        let (kind, role) = match scheme {
            "gpt" => parse_guid(entry_type).map(gpt_kind),
            "dos" => u8::from_str_radix(entry_type.trim_start_matches("0x"), 16)
                .ok()
                .map(mbr_kind),
            _ => None,
        }
        .unwrap_or_default();
        if role == PartitionRole::Extended {
            continue;
        }

        // What is built on it, and mounted from any of that.
        let above = holders(&device);
        let stack: Vec<&str> = std::iter::once(device.as_str())
            .chain(above.iter().map(String::as_str))
            .collect();
        let mut points: Vec<String> = mounts
            .iter()
            .filter(|m| stack.contains(&m.device.as_str()))
            .map(|m| m.point.clone())
            .collect();
        let (mut total, mut free) = stack
            .iter()
            .filter_map(|d| spaces.get(*d))
            .fold((0, 0), |(t, f), s| (t + s.0, f + s.1));
        for (size, used) in stack.iter().filter_map(|d| swaps.get(*d)) {
            points.push("[SWAP]".into());
            total += size;
            free += size.saturating_sub(*used);
        }

        let mut fs = fact("ID_FS_TYPE").to_string();
        if fs.is_empty() {
            fs = mounts
                .iter()
                .find(|m| m.device == device)
                .map_or(String::new(), |m| m.fs_type.clone());
        }
        let mut file_system = file_system_name(&fs, fact("ID_FS_VERSION"));
        let mut label = unescape(fact("ID_FS_LABEL_ENC"));
        if label.is_empty() {
            label = labels.get(&device).cloned().unwrap_or_default();
        }
        // Inside encryption or a volume manager: what the mappings hold.
        let mut inner: Vec<String> = Vec::new();
        for holder in &above {
            let facts = read(format!("/sys/class/block/{holder}/dev"))
                .map(|d| udev(&d))
                .unwrap_or_default();
            let fact = |key: &str| facts.get(key).map_or("", |s| s.as_str());
            let fs = fact("ID_FS_TYPE");
            if fs.is_empty() || CONTAINERS.contains(&fs) {
                continue;
            }
            let name = file_system_name(fs, fact("ID_FS_VERSION"));
            if !inner.contains(&name) {
                inner.push(name);
            }
            if label.is_empty() {
                label = unescape(fact("ID_FS_LABEL_ENC"));
            }
        }
        if !inner.is_empty() {
            file_system = format!("{} in {file_system}", inner.join(", "));
        }

        let name = match fact("ID_PART_ENTRY_NAME") {
            // Without udev, the kernel knows GPT names too.
            "" => std::fs::read_to_string(path.join("uevent"))
                .ok()
                .and_then(|u| {
                    u.lines()
                        .find_map(|l| l.strip_prefix("PARTNAME="))
                        .map(str::to_string)
                })
                .unwrap_or_default(),
            name => unescape(name),
        };
        out.push(Partition {
            number,
            offset: bytes("start"),
            size: bytes("size"),
            role: role_from_contents(role, &fs, &points),
            kind,
            name,
            label,
            file_system,
            system: points.iter().any(|p| p == "/"),
            mounts: points,
            total,
            free,
            device,
        });
    }
    out.sort_by_key(|p| p.offset);
    (style, out)
}

/// Size and room left of the file system mounted at `mount`, for ordinary
/// users (the root reserve not counted as free).
fn space(mount: &str) -> Option<(u64, u64)> {
    let path = std::ffi::CString::new(mount).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let unit = st.f_frsize as u64;
    Some((st.f_blocks as u64 * unit, st.f_bavail as u64 * unit))
}

fn diskstats() -> HashMap<String, Counters> {
    let Ok(text) = std::fs::read_to_string("/proc/diskstats") else {
        return HashMap::new();
    };
    let at = Instant::now();
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_ascii_whitespace().collect();
            let n = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            Some((
                f.get(2)?.to_string(),
                Counters {
                    reads: n(3),
                    sectors_read: n(5),
                    ms_reading: n(6),
                    writes: n(7),
                    sectors_written: n(9),
                    ms_writing: n(10),
                    in_flight: n(11),
                    ms_active: n(12),
                    at,
                },
            ))
        })
        .collect()
}

/// The physical disks' kernel names, in the order they are numbered (the
/// disk trace numbers them the same way).
pub(crate) fn physical_disks() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir("/sys/block")
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| physical(n))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

impl DiskSampler {
    pub fn new() -> Self {
        let disks = physical_disks()
            .into_iter()
            .enumerate()
            .map(|(number, name)| Disk {
                stats: identify(&name, number as u32),
                name,
                prev: None,
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
            let mounts = mounts();
            let spaces = spaces(&mounts);
            let labels = labels();
            let swaps = swaps();
            let vols = volumes(&mounts, &spaces, &labels);
            for disk in &mut self.disks {
                disk.stats.volumes = vols
                    .iter()
                    .filter(|(d, _)| *d == disk.name)
                    .map(|(_, v)| v.clone())
                    .collect();
                (disk.stats.style, disk.stats.partitions) =
                    partitions(&disk.name, &mounts, &spaces, &swaps, &labels);
            }
        }
        let read_temperature = self
            .temperature_at
            .is_none_or(|t| now - t > Duration::from_secs(15));
        if read_temperature {
            self.temperature_at = Some(now);
        }
        let stats = diskstats();
        for disk in &mut self.disks {
            if read_temperature {
                disk.stats.temperature = temperature(&disk.name);
            }
            let Some(&now) = stats.get(&disk.name) else {
                continue;
            };
            let s = &mut disk.stats;
            s.queue = now.in_flight as u32;
            if let Some(prev) = disk.prev {
                let secs = (now.at - prev.at).as_secs_f64().max(1e-3);
                let reads = now.reads.saturating_sub(prev.reads) as f64;
                let writes = now.writes.saturating_sub(prev.writes) as f64;
                s.read_rate =
                    now.sectors_read.saturating_sub(prev.sectors_read) as f64 * 512.0 / secs;
                s.write_rate =
                    now.sectors_written.saturating_sub(prev.sectors_written) as f64 * 512.0 / secs;
                s.read_iops = reads / secs;
                s.write_iops = writes / secs;
                let active_ms = now.ms_active.saturating_sub(prev.ms_active) as f64;
                s.active = (active_ms / (secs * 10.0)).clamp(0.0, 100.0) as f32;
                let busy = (now.ms_reading + now.ms_writing)
                    .saturating_sub(prev.ms_reading + prev.ms_writing)
                    as f64;
                s.response_ms = if reads + writes > 0.0 {
                    (busy / (reads + writes)) as f32
                } else {
                    0.0
                };
            }
            disk.prev = Some(now);
        }
        self.disks.iter().map(|d| d.stats.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_disks_and_the_root_file_system() {
        let mut sampler = DiskSampler::new();
        let first = sampler.sample();
        assert!(!first.is_empty(), "no physical disk");
        assert!(first.iter().all(|d| d.size > 0 && !d.model.is_empty()));
        assert!(
            first
                .iter()
                .flat_map(|d| &d.volumes)
                .any(|v| v.letter == "/" && v.total > 0),
            "/ is on none of them"
        );
    }

    #[test]
    fn reads_the_partitions() {
        let mut sampler = DiskSampler::new();
        let disks = sampler.sample();
        for d in &disks {
            println!("{} {} {:?}", d.number, d.model, d.style);
            for p in &d.partitions {
                println!(
                    "  {:>2} {:<10} {:>14} {:>14}  {:<22} {:?} name={:?} label={:?} fs={:?} \
                     mounts={:?} total={} free={} system={}",
                    p.number,
                    p.device,
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
            let mut end = 0;
            for p in &d.partitions {
                assert!(p.offset >= end, "partitions overlap: {p:?}");
                assert!(p.size > 0);
                end = p.offset + p.size;
            }
            assert!(end <= d.size, "partitions run past the disk");
        }
        let system: Vec<&Partition> = disks
            .iter()
            .flat_map(|d| &d.partitions)
            .filter(|p| p.system)
            .collect();
        // / on a partition, or on a mapping over one (LUKS, LVM).
        if disks.iter().any(|d| !d.partitions.is_empty()) {
            assert_eq!(system.len(), 1, "{system:?}");
            assert!(system[0].total > 0);
        }
    }

    #[test]
    fn reads_contents_over_types() {
        let mounts = ["/boot".to_string()];
        assert_eq!(
            role_from_contents(PartitionRole::Data, "swap", &[]),
            PartitionRole::Swap
        );
        assert_eq!(
            role_from_contents(PartitionRole::Data, "vfat", &mounts),
            PartitionRole::Boot
        );
        assert_eq!(
            role_from_contents(PartitionRole::Recovery, "ntfs", &[]),
            PartitionRole::Recovery
        );
        assert_eq!(file_system_name("vfat", "FAT32"), "FAT32");
        assert_eq!(file_system_name("crypto_LUKS", "2"), "LUKS");
    }

    #[test]
    fn unescapes_labels_and_mount_points() {
        assert_eq!(unescape(r"New\x20Volume"), "New Volume");
        assert_eq!(unescape(r"Caf\xc3\xa9"), "Café");
        assert_eq!(unescape_octal(r"/mnt/my\040disk"), "/mnt/my disk");
    }
}
