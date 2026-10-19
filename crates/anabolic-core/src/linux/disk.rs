//! Physical disks from /sys/block and /proc/diskstats: what each one is,
//! how busy, its temperature, and the file systems mounted from it.

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
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("\\x") {
        out.push_str(&rest[..at]);
        match rest
            .get(at + 2..at + 4)
            .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            Some(b) => {
                out.push(b as char);
                rest = &rest[at + 4..];
            }
            None => {
                out.push_str("\\x");
                rest = &rest[at + 2..];
            }
        }
    }
    out.push_str(rest);
    out
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

/// Every mounted file system on a disk: (disk name, volume).
fn volumes() -> Vec<(String, Volume)> {
    let Ok(table) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    let labels = labels();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in table.lines() {
        // id parent major:minor root mount-point options ... - type source super-options
        let fields: Vec<&str> = line.split(' ').collect();
        let Some(dash) = fields.iter().position(|f| *f == "-") else {
            continue;
        };
        let (Some(number), Some(mount)) = (fields.get(2), fields.get(4)) else {
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
        // Bind mounts and subvolumes show a file system again: the first
        // mount of it (usually the whole of it) is enough.
        if !seen.insert(device.clone()) {
            continue;
        }
        let mount = unescape_octal(mount);
        let (total, free) = space(&mount).unwrap_or((0, 0));
        let fs_type = fields.get(dash + 1).copied().unwrap_or("");
        for disk in disks_under(&device) {
            out.push((
                disk,
                Volume {
                    letter: mount.clone(),
                    label: labels
                        .get(&device)
                        .cloned()
                        .unwrap_or_else(|| fs_type.to_string()),
                    total,
                    free,
                },
            ));
        }
    }
    out
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

impl DiskSampler {
    pub fn new() -> Self {
        let mut names: Vec<String> = std::fs::read_dir("/sys/block")
            .map(|d| {
                d.flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| physical(n))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        let disks = names
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
            let vols = volumes();
            for disk in &mut self.disks {
                disk.stats.volumes = vols
                    .iter()
                    .filter(|(d, _)| *d == disk.name)
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
    fn unescapes_labels_and_mount_points() {
        assert_eq!(unescape(r"New\x20Volume"), "New Volume");
        assert_eq!(unescape_octal(r"/mnt/my\040disk"), "/mnt/my disk");
    }
}
