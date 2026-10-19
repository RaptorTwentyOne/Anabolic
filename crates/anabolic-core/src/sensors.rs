//! Temperatures, fan speeds and power draw, as the hardware reports them.
//!
//! - Linux has a driver for most sensor chips (/sys/class/hwmon) and the
//!   firmware's thermal zones: the processor's package and cores, memory
//!   modules, network adapters, the mainboard's or laptop's fans.
//! - Windows gives the processor's own sensors only to drivers, and
//!   Anabolic installs none: what it can read are the firmware's (ACPI)
//!   thermal zones.
//!
//! GPUs and disks report through their own drivers on both, and their
//! samplers read them (`gpu`, `disk`); `merge` puts everything in one list.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use arc_swap::ArcSwap;

use crate::disk::DiskStats;
use crate::gpu::AdapterGpuStats;
use crate::hardening::{ThreadPriority, set_current_thread_priority};

#[cfg_attr(windows, path = "windows/sensors.rs")]
#[cfg_attr(target_os = "linux", path = "linux/sensors.rs")]
mod imp;
pub use imp::*;

/// The device the processor's own sensors are listed under.
pub const CPU: &str = "CPU";

/// How often sensors are read: often enough to follow a load, rarely
/// enough that slow ones (an embedded controller, a memory module's
/// SMBus) cost nothing.
const EVERY: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SensorKind {
    /// Degrees Celsius.
    Temperature,
    /// Revolutions per minute.
    Fan,
    /// Watts.
    Power,
}

impl SensorKind {
    pub fn unit(self) -> &'static str {
        match self {
            Self::Temperature => "°C",
            Self::Fan => "RPM",
            Self::Power => "W",
        }
    }
}

/// How worrying a reading is.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Heat {
    Normal,
    /// Within 20 °C of where the hardware throttles or shuts down.
    Warm,
    /// Within 10 °C of it.
    Hot,
}

#[derive(Clone, Debug)]
pub struct Sensor {
    /// What it sits on: "CPU", a GPU's or disk's name, "Memory", "Wi-Fi".
    pub device: Arc<str>,
    /// Which one on the device: "Package", "Core 8 (CPU 2, 3)",
    /// "Composite", "Fan 1".
    pub label: Arc<str>,
    pub kind: SensorKind,
    /// In the kind's unit.
    pub value: f32,
    /// Where the hardware considers it hot (an alarm, fans at full speed).
    pub high: Option<f32>,
    /// Where it throttles hard or shuts down.
    pub critical: Option<f32>,
}

impl Sensor {
    /// How close a temperature is to its limit; fans and power have none.
    pub fn heat(&self) -> Heat {
        match self.kind {
            SensorKind::Temperature => heat(self.value, self.high, self.critical),
            _ => Heat::Normal,
        }
    }
}

/// How close a temperature is to the hardware's limit: its critical
/// mark, else a little above its high one, else 100 °C (where processors
/// throttle).
pub fn heat(value: f32, high: Option<f32>, critical: Option<f32>) -> Heat {
    let limit = critical.or(high.map(|h| h + 5.0)).unwrap_or(100.0);
    if value >= limit - 10.0 {
        Heat::Hot
    } else if value >= limit - 20.0 {
        Heat::Warm
    } else {
        Heat::Normal
    }
}

/// The processor's temperature: its package (Intel), else its die or
/// control temperature (AMD); the hottest when there are several.
pub fn cpu_sensor(sensors: &[Sensor]) -> Option<&Sensor> {
    let of_cpu = || {
        sensors
            .iter()
            .filter(|s| s.kind == SensorKind::Temperature && &*s.device == CPU)
    };
    ["Package", "Tdie", "Tctl", ""].iter().find_map(|prefix| {
        of_cpu()
            .filter(|s| s.label.starts_with(prefix))
            .max_by(|a, b| a.value.total_cmp(&b.value))
    })
}

/// Everything in one list, grouped by device: the processor first, then
/// the GPUs and disks as their own samplers read them (unless the sensor
/// chips already listed them), then the rest.
pub(crate) fn merge(read: &[Sensor], gpus: &[AdapterGpuStats], disks: &[DiskStats]) -> Vec<Sensor> {
    let listed = |device: &str| read.iter().any(|s| &*s.device == device);
    let mut out: Vec<Sensor> = read.iter().filter(|s| &*s.device == CPU).cloned().collect();
    for g in gpus.iter().filter(|g| !listed(&g.name)) {
        out.extend(g.sensors.iter().cloned());
    }
    out.extend(read.iter().filter(|s| &*s.device != CPU).cloned());
    for d in disks.iter().filter(|d| !listed(&d.model)) {
        if let Some(t) = d.temperature {
            out.push(Sensor {
                device: Arc::from(d.model.as_str()),
                label: Arc::from("Drive"),
                kind: SensorKind::Temperature,
                value: t as f32,
                high: None,
                critical: None,
            });
        }
    }
    out
}

/// Reads the sensors every few seconds on a thread of its own, so a slow
/// one never holds up the collector.
pub(crate) fn spawn(out: Arc<ArcSwap<Vec<Sensor>>>) {
    let _ = thread::Builder::new()
        .name("anabolic-sensors".into())
        .spawn(move || {
            set_current_thread_priority(ThreadPriority::BelowNormal);
            let mut sampler = SensorSampler::new();
            loop {
                out.store(Arc::new(sampler.sample()));
                thread::sleep(EVERY);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sensor(device: &str, label: &str, value: f32) -> Sensor {
        Sensor {
            device: Arc::from(device),
            label: Arc::from(label),
            kind: SensorKind::Temperature,
            value,
            high: None,
            critical: None,
        }
    }

    #[test]
    fn heat_follows_the_limits() {
        // A processor that throttles at 100 °C.
        assert_eq!(heat(70.0, Some(100.0), Some(100.0)), Heat::Normal);
        assert_eq!(heat(85.0, Some(100.0), Some(100.0)), Heat::Warm);
        assert_eq!(heat(95.0, Some(100.0), Some(100.0)), Heat::Hot);
        // An NVMe drive: high 76.85, critical 79.85.
        assert_eq!(heat(37.0, Some(76.85), Some(79.85)), Heat::Normal);
        assert_eq!(heat(72.0, Some(76.85), Some(79.85)), Heat::Hot);
        // Only a high mark; nothing at all.
        assert_eq!(heat(80.0, Some(80.0), None), Heat::Hot);
        assert_eq!(heat(60.0, None, None), Heat::Normal);
        assert_eq!(heat(81.0, None, None), Heat::Warm);
        let fan = Sensor {
            kind: SensorKind::Fan,
            ..sensor("System", "Fan 1", 2400.0)
        };
        assert_eq!(fan.heat(), Heat::Normal);
    }

    #[test]
    fn the_package_speaks_for_the_cpu() {
        let list = [
            sensor(CPU, "Core 0", 80.0),
            sensor(CPU, "Package", 70.0),
            sensor("Thermal zones", "Package", 99.0),
        ];
        assert_eq!(cpu_sensor(&list).unwrap().value, 70.0);
        let amd = [sensor(CPU, "Tccd1", 60.0), sensor(CPU, "Tctl", 65.0)];
        assert_eq!(&*cpu_sensor(&amd).unwrap().label, "Tctl");
        assert!(cpu_sensor(&[sensor("ACPI", "THRM", 50.0)]).is_none());
    }

    #[test]
    fn merges_gpus_and_disks_once() {
        let read = [
            sensor("Wi-Fi", "Temperature", 40.0),
            sensor(CPU, "Package", 60.0),
            sensor("SOLIDIGM SSD", "Composite", 37.0),
        ];
        let gpus = [AdapterGpuStats {
            name: Arc::from("GPU"),
            sensors: vec![sensor("GPU", "Temperature", 50.0)],
            ..Default::default()
        }];
        let disks = [
            DiskStats {
                model: "SOLIDIGM SSD".into(),
                temperature: Some(36),
                ..Default::default()
            },
            DiskStats {
                model: "Other".into(),
                temperature: Some(30),
                ..Default::default()
            },
        ];
        let merged = merge(&read, &gpus, &disks);
        let names: Vec<(&str, &str)> = merged.iter().map(|s| (&*s.device, &*s.label)).collect();
        assert_eq!(
            names,
            [
                (CPU, "Package"),
                ("GPU", "Temperature"),
                ("Wi-Fi", "Temperature"),
                ("SOLIDIGM SSD", "Composite"),
                ("Other", "Drive"),
            ]
        );
    }
}
