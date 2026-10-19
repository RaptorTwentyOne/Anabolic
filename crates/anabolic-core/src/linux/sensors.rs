//! The kernel's hardware monitors (/sys/class/hwmon), and the firmware's
//! thermal zones (/sys/class/thermal) that have none.
//!
//! A sensor on a PCI device that sleeps is skipped rather than woken. GPUs
//! are left to the GPU sampler and SATA disks to the disk sampler: they
//! read their own.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;

/// Devices come and go (a dock, a module loaded): look again now and then.
const RELIST: Duration = Duration::from_secs(60);
/// NVMe drives answer from their controller, which a question keeps out
/// of its deepest power state for a moment.
const SLOW: Duration = Duration::from_secs(10);

/// Where a device's group goes in the list.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Rank {
    Cpu,
    Memory,
    Disk,
    Network,
    Board,
    Zone,
    Battery,
    Other,
}

/// One value to read, and what it means.
pub(crate) struct Source {
    input: PathBuf,
    /// The power state of the PCI device it sits on.
    power: Option<PathBuf>,
    device: Arc<str>,
    label: Arc<str>,
    kind: SensorKind,
    /// File units per sensor unit: millidegrees, microwatts.
    scale: f64,
    high: Option<f32>,
    critical: Option<f32>,
    /// A fan without a name may not be connected; it is listed once it
    /// has turned.
    unnamed_fan: bool,
    slow: bool,
    last: Option<(Instant, Option<Sensor>)>,
}

impl Source {
    /// The value now; None when the device sleeps or does not answer.
    pub(crate) fn read(&self) -> Option<Sensor> {
        if self.power.as_deref().is_some_and(asleep) {
            return None;
        }
        let value = (number(&self.input)? as f64 / self.scale) as f32;
        // A probe that is not connected reads -128 or 255.
        if self.kind == SensorKind::Temperature && !(-40.0..=200.0).contains(&value) {
            return None;
        }
        Some(Sensor {
            device: self.device.clone(),
            label: self.label.clone(),
            kind: self.kind,
            value,
            high: self.high,
            critical: self.critical,
        })
    }
}

fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

fn number(path: &Path) -> Option<i64> {
    read(path)?.parse().ok()
}

/// A limit in millidegrees, when it is a real one.
fn limit(path: &Path) -> Option<f32> {
    let c = number(path)? as f32 / 1000.0;
    (c > 0.0 && c <= 200.0).then_some(c)
}

/// A PCI device that runtime power management has put to sleep.
fn asleep(status: &Path) -> bool {
    read(status).is_some_and(|s| s == "suspended" || s == "suspending")
}

/// The PCI device a device sits on, if any.
fn pci_device(device: &Path) -> Option<&Path> {
    device.ancestors().find(|dir| {
        std::fs::read_link(dir.join("subsystem"))
            .is_ok_and(|s| s.file_name().is_some_and(|n| n == "pci"))
    })
}

/// The power state to check before reading a device's sensors: its PCI
/// device's. Not an SMBus controller's (memory modules hang off it): it
/// wakes in an instant and sleeps again.
fn pci_power(device: &Path) -> Option<PathBuf> {
    let pci = pci_device(device)?;
    let smbus = read(pci.join("class")).is_some_and(|c| c.starts_with("0x0c05"));
    (!smbus).then(|| pci.join("power/runtime_status"))
}

/// "Engine" for "engine", "Hot spot" for amdgpu's "junction".
fn tidy(label: &str) -> String {
    match label {
        "junction" => "Hot spot".into(),
        "mem" | "vram" => "Memory".into(),
        "pkg" => "Package".into(),
        _ => {
            let mut chars = label.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        }
    }
}

/// Logical processors per (package, core id), from the topology.
fn core_cpus() -> HashMap<(u32, u32), Vec<usize>> {
    let mut map: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    let Ok(dir) = std::fs::read_dir("/sys/devices/system/cpu") else {
        return map;
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(cpu) = name
            .to_str()
            .and_then(|n| n.strip_prefix("cpu"))
            .and_then(|n| n.parse::<usize>().ok())
        else {
            continue;
        };
        let topology = entry.path().join("topology");
        let (Some(package), Some(core)) = (
            number(&topology.join("physical_package_id")),
            number(&topology.join("core_id")),
        ) else {
            continue;
        };
        map.entry((package as u32, core as u32))
            .or_default()
            .push(cpu);
    }
    for cpus in map.values_mut() {
        cpus.sort_unstable();
    }
    map
}

/// coretemp's labels: "Package id 0" as "Package", "Core 8" with the
/// logical processors that share it.
fn coretemp_label(label: &str, package: u32, cores: &HashMap<(u32, u32), Vec<usize>>) -> String {
    if let Some(id) = label.strip_prefix("Package id ") {
        return match id.trim() {
            "0" => "Package".into(),
            id => format!("Package {id}"),
        };
    }
    if let Some(core) = label
        .strip_prefix("Core ")
        .and_then(|c| c.parse::<u32>().ok())
        && let Some(cpus) = cores.get(&(package, core))
    {
        let list: Vec<String> = cpus.iter().map(usize::to_string).collect();
        return format!("{label} (CPU {})", list.join(", "));
    }
    label.to_string()
}

/// The temperatures, fans and power readings of one hardware monitor.
/// `label` names one that has no label of its own (kind, index, how many
/// of that kind).
pub(crate) fn hwmon_sources(
    dir: &Path,
    device: &Arc<str>,
    power: Option<PathBuf>,
    label: impl Fn(SensorKind, u32, usize) -> String,
) -> Vec<Source> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    // (kind, index) -> input file; a power reading may be an average.
    let mut inputs: Vec<(SensorKind, u32, String)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let (kind, rest) = if let Some(rest) = name.strip_prefix("temp") {
            (SensorKind::Temperature, rest)
        } else if let Some(rest) = name.strip_prefix("fan") {
            (SensorKind::Fan, rest)
        } else if let Some(rest) = name.strip_prefix("power") {
            (SensorKind::Power, rest)
        } else {
            continue;
        };
        let Some((index, suffix)) = rest.split_once('_') else {
            continue;
        };
        let Ok(index) = index.parse::<u32>() else {
            continue;
        };
        let wanted = suffix == "input" || (kind == SensorKind::Power && suffix == "average");
        if !wanted {
            continue;
        }
        match inputs
            .iter_mut()
            .find(|(k, i, _)| *k == kind && *i == index)
        {
            // A power reading's current value beats its average.
            Some(existing) if suffix == "input" => existing.2 = name.to_string(),
            Some(_) => {}
            None => inputs.push((kind, index, name.to_string())),
        }
    }
    inputs.sort_by_key(|(kind, index, _)| (*kind as u8, *index));
    let count = |kind| inputs.iter().filter(|(k, ..)| *k == kind).count();
    inputs
        .iter()
        .map(|(kind, index, file)| {
            let prefix = match kind {
                SensorKind::Temperature => "temp",
                SensorKind::Fan => "fan",
                SensorKind::Power => "power",
            };
            let own = read(dir.join(format!("{prefix}{index}_label"))).filter(|l| !l.is_empty());
            let (high, critical) = if *kind == SensorKind::Temperature {
                (
                    limit(&dir.join(format!("temp{index}_max"))),
                    limit(&dir.join(format!("temp{index}_crit")))
                        .or_else(|| limit(&dir.join(format!("temp{index}_emergency")))),
                )
            } else {
                (None, None)
            };
            Source {
                input: dir.join(file),
                power: power.clone(),
                device: device.clone(),
                label: Arc::from(match &own {
                    Some(l) => tidy(l),
                    None => label(*kind, *index, count(*kind)),
                }),
                kind: *kind,
                scale: match kind {
                    SensorKind::Temperature => 1000.0,
                    SensorKind::Fan => 1.0,
                    SensorKind::Power => 1_000_000.0,
                },
                high,
                critical,
                unnamed_fan: *kind == SensorKind::Fan && own.is_none(),
                slow: false,
                last: None,
            }
        })
        .collect()
}

/// "Temperature", "Temperature 2", "Fan 1", "Power".
pub(crate) fn plain_label(kind: SensorKind, index: u32, of_kind: usize) -> String {
    let name = match kind {
        SensorKind::Temperature => "Temperature",
        SensorKind::Fan => "Fan",
        SensorKind::Power => "Power",
    };
    if of_kind > 1 || kind == SensorKind::Fan {
        format!("{name} {index}")
    } else {
        name.to_string()
    }
}

/// A disk's name as the disk sampler gives it: its model, after its
/// vendor unless that is just "ATA".
fn disk_name(device: &Path) -> Option<String> {
    let model = read(device.join("model")).filter(|m| !m.is_empty())?;
    let vendor = read(device.join("vendor")).filter(|v| !v.is_empty() && v != "ATA");
    Some(match vendor {
        Some(vendor) if !model.starts_with(&vendor) => format!("{vendor} {model}"),
        _ => model,
    })
}

/// A network adapter on this PCI device: Wi-Fi or wired.
fn network_kind(pci: &Path) -> Option<&'static str> {
    let interfaces = std::fs::read_dir(pci.join("net")).ok()?;
    let mut wired = false;
    for interface in interfaces.flatten() {
        let path = interface.path();
        if path.join("wireless").exists() || path.join("phy80211").exists() {
            return Some("Wi-Fi");
        }
        wired = true;
    }
    wired.then_some("Ethernet")
}

/// Thresholds of a thermal zone from its trip points: where it starts
/// cooling hard ("hot", else the first "passive"), and "critical".
fn trip_points(zone: &Path) -> (Option<f32>, Option<f32>) {
    let (mut hot, mut passive, mut critical) = (None, None, None);
    for i in 0.. {
        let Some(kind) = read(zone.join(format!("trip_point_{i}_type"))) else {
            break;
        };
        let temp = limit(&zone.join(format!("trip_point_{i}_temp")));
        match kind.as_str() {
            "critical" => critical = critical.or(temp),
            "hot" => hot = hot.or(temp),
            "passive" => passive = passive.or(temp),
            _ => {}
        }
    }
    (hot.or(passive), critical)
}

const CPU_MONITORS: [&str; 6] = [
    "coretemp",
    "k10temp",
    "k8temp",
    "zenpower",
    "fam15h_power",
    "via_cputemp",
];
const MEMORY_MONITORS: [&str; 3] = ["spd5118", "jc42", "ee1004"];
const WIFI_MONITORS: [&str; 5] = ["iwlwifi", "ath", "mt7", "rtw", "brcmfmac"];
/// Embedded controllers and Super I/O chips: the fans, the board.
const BOARD_MONITORS: [&str; 16] = [
    "thinkpad", "dell_smm", "applesmc", "asus", "hp", "nct", "it87", "it86", "f71", "w83", "sch5",
    "gigabyte", "legion", "msi", "acer", "lenovo",
];

/// What one hardware monitor sits on, and where its group goes. None for
/// what other samplers read (GPUs, SATA disks) and power supplies.
fn describe(name: &str, device: &Path) -> Option<(String, Rank)> {
    let starts = |list: &[&str]| list.iter().any(|p| name.starts_with(p));
    if device.join("drm").is_dir() || name == "drivetemp" {
        return None;
    }
    if CPU_MONITORS.contains(&name) {
        return Some((CPU.into(), Rank::Cpu));
    }
    if name == "nvme" {
        return Some((disk_name(device)?, Rank::Disk));
    }
    if MEMORY_MONITORS.contains(&name) {
        return Some(("Memory".into(), Rank::Memory));
    }
    if device.to_string_lossy().contains("/power_supply/") {
        let battery = read(device.join("type")).is_some_and(|t| t == "Battery");
        return battery.then(|| ("Battery".into(), Rank::Battery));
    }
    if starts(&WIFI_MONITORS) {
        return Some(("Wi-Fi".into(), Rank::Network));
    }
    if let Some(kind) = pci_device(device).and_then(network_kind) {
        return Some((kind.into(), Rank::Network));
    }
    if name == "acpitz" {
        return Some(("Thermal zones".into(), Rank::Zone));
    }
    if name.starts_with("pch_") {
        return Some(("Chipset".into(), Rank::Board));
    }
    if starts(&BOARD_MONITORS) {
        return Some(("System".into(), Rank::Board));
    }
    Some((name.to_string(), Rank::Other))
}

/// Everything to read, grouped and in order.
fn sources() -> Vec<Source> {
    let mut monitors: Vec<(Rank, String, u32, PathBuf, PathBuf, String)> = Vec::new();
    if let Ok(dir) = std::fs::read_dir("/sys/class/hwmon") {
        for entry in dir.flatten() {
            let path = entry.path();
            let Some(n) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.strip_prefix("hwmon")?.parse::<u32>().ok())
            else {
                continue;
            };
            let Some(name) = read(path.join("name")) else {
                continue;
            };
            // A virtual monitor has no device: it stands for itself.
            let device =
                std::fs::canonicalize(path.join("device")).unwrap_or_else(|_| path.clone());
            if let Some((group, rank)) = describe(&name, &device) {
                monitors.push((rank, group, n, path, device, name));
            }
        }
    }
    monitors.sort_by(|a, b| (a.0, &a.1, a.2).cmp(&(b.0, &b.1, b.2)));

    let cores = core_cpus();
    let mut out = Vec::new();
    let mut modules = 0;
    let mut zones = 0;
    let mut has_coretemp = false;
    for (_, group, _, path, device, name) in &monitors {
        let group: Arc<str> = Arc::from(group.as_str());
        let power = pci_power(device);
        let mut found = match name.as_str() {
            "coretemp" => {
                has_coretemp = true;
                // Its labels name the package; coretemp.N is the N-th.
                let package = std::fs::read_dir(path)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().ends_with("_label"))
                    .find_map(|e| {
                        read(e.path())?
                            .strip_prefix("Package id ")?
                            .parse::<u32>()
                            .ok()
                    })
                    .unwrap_or(0);
                let mut found = hwmon_sources(path, &group, power, plain_label);
                for s in &mut found {
                    s.label = Arc::from(coretemp_label(&s.label, package, &cores));
                }
                found
            }
            _ if MEMORY_MONITORS.contains(&name.as_str()) => {
                modules += 1;
                hwmon_sources(path, &group, power, |kind, index, of_kind| {
                    match (kind, of_kind) {
                        (SensorKind::Temperature, 1) => format!("Module {modules}"),
                        _ => format!("Module {modules} {}", plain_label(kind, index, of_kind)),
                    }
                })
            }
            "acpitz" => {
                zones += 1;
                let mut found = hwmon_sources(path, &group, power, |kind, index, of_kind| {
                    match (kind, of_kind) {
                        (SensorKind::Temperature, 1) => "ACPI".into(),
                        _ => format!("ACPI {}", plain_label(kind, index, of_kind)),
                    }
                });
                if zones > 1 {
                    for s in &mut found {
                        s.label = Arc::from(format!("{} {zones}", s.label));
                    }
                }
                found
            }
            _ => hwmon_sources(path, &group, power, plain_label),
        };
        // A monitor of a thermal zone takes the zone's trip points.
        if device.join("trip_point_0_type").exists() {
            let (high, critical) = trip_points(device);
            for s in found
                .iter_mut()
                .filter(|s| s.kind == SensorKind::Temperature)
            {
                s.high = s.high.or(high);
                s.critical = s.critical.or(critical);
            }
        }
        if name == "nvme" {
            for s in &mut found {
                s.slow = true;
            }
        }
        out.extend(found);
    }

    // Thermal zones without a hardware monitor of their own.
    let mut zones: Vec<(u32, PathBuf)> = std::fs::read_dir("/sys/class/thermal")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e
                .file_name()
                .to_str()?
                .strip_prefix("thermal_zone")?
                .parse()
                .ok()?;
            Some((n, e.path()))
        })
        .collect();
    zones.sort();
    let zone_group: Arc<str> = Arc::from("Thermal zones");
    let mut zone_sources = Vec::new();
    for (_, zone) in zones {
        let has_hwmon = std::fs::read_dir(&zone)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("hwmon"));
        let Some(kind) = read(zone.join("type")) else {
            continue;
        };
        // INT3400 is the platform's policy, with a fixed reading.
        if has_hwmon || read(zone.join("mode")).is_some_and(|m| m == "disabled") {
            continue;
        }
        let (device, label) = match kind.as_str() {
            "INT3400 Thermal" => continue,
            // The package temperature, which coretemp gives already.
            "x86_pkg_temp" if has_coretemp => continue,
            "x86_pkg_temp" => (Arc::<str>::from(CPU), "Package".to_string()),
            _ => (zone_group.clone(), kind),
        };
        let (high, critical) = trip_points(&zone);
        let source = Source {
            input: zone.join("temp"),
            power: None,
            device,
            label: Arc::from(label),
            kind: SensorKind::Temperature,
            scale: 1000.0,
            high,
            critical,
            unnamed_fan: false,
            slow: false,
            last: None,
        };
        if &*source.device == CPU {
            out.insert(0, source);
        } else {
            zone_sources.push(source);
        }
    }
    // After the zones that do have a monitor (acpitz), before batteries.
    let at = out
        .iter()
        .rposition(|s| s.device == zone_group)
        .map(|i| i + 1)
        .or_else(|| out.iter().position(|s| &*s.device == "Battery"))
        .unwrap_or(out.len());
    out.splice(at..at, zone_sources);
    out
}

pub struct SensorSampler {
    sources: Vec<Source>,
    listed: Option<Instant>,
    /// Unnamed fans seen turning: one that stops stays listed.
    spun: HashSet<PathBuf>,
}

impl Default for SensorSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl SensorSampler {
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
            listed: None,
            spun: HashSet::new(),
        }
    }

    pub fn sample(&mut self) -> Vec<Sensor> {
        let now = Instant::now();
        if self.listed.is_none_or(|t| now - t > RELIST) {
            self.sources = sources();
            self.listed = Some(now);
        }
        let mut out = Vec::with_capacity(self.sources.len());
        for source in &mut self.sources {
            let sensor = match &source.last {
                Some((at, last)) if source.slow && now - *at < SLOW => last.clone(),
                _ => {
                    let sensor = source.read();
                    if source.slow {
                        source.last = Some((now, sensor.clone()));
                    }
                    sensor
                }
            };
            let Some(sensor) = sensor else {
                continue;
            };
            if source.unnamed_fan {
                if sensor.value > 0.0 {
                    self.spun.insert(source.input.clone());
                } else if !self.spun.contains(&source.input) {
                    continue;
                }
            }
            out.push(sensor);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_read_well() {
        assert_eq!(tidy("junction"), "Hot spot");
        assert_eq!(tidy("edge"), "Edge");
        assert_eq!(tidy("Composite"), "Composite");
        assert_eq!(plain_label(SensorKind::Temperature, 1, 1), "Temperature");
        assert_eq!(plain_label(SensorKind::Temperature, 3, 2), "Temperature 3");
        assert_eq!(plain_label(SensorKind::Fan, 1, 1), "Fan 1");
        let cores = HashMap::from([((0, 8), vec![2, 3]), ((0, 32), vec![16])]);
        assert_eq!(coretemp_label("Package id 0", 0, &cores), "Package");
        assert_eq!(coretemp_label("Package id 1", 1, &cores), "Package 1");
        assert_eq!(coretemp_label("Core 8", 0, &cores), "Core 8 (CPU 2, 3)");
        assert_eq!(coretemp_label("Core 32", 0, &cores), "Core 32 (CPU 16)");
        assert_eq!(coretemp_label("Core 9", 0, &cores), "Core 9");
    }

    #[test]
    fn reads_this_machines_sensors() {
        let mut sampler = SensorSampler::new();
        let sensors = sampler.sample();
        for s in &sensors {
            println!(
                "{:<24} {:<24} {:>8.1} {:<3} high {:?} crit {:?}",
                s.device,
                s.label,
                s.value,
                s.kind.unit(),
                s.high,
                s.critical
            );
        }
        for s in &sensors {
            assert!(!s.device.is_empty() && !s.label.is_empty());
            if s.kind == SensorKind::Temperature {
                assert!((-40.0..=200.0).contains(&s.value), "{s:?}");
            }
        }
        // A second look reads the same sensors.
        assert!(sampler.sample().len() <= sensors.len() + 2);
    }
}
