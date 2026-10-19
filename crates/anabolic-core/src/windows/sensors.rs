//! The firmware's thermal zones, from the performance counters (no admin
//! rights needed).
//!
//! Windows gives the processor's own sensors (its package and cores) only
//! to kernel drivers, and Anabolic installs none. The thermal zones are
//! what the firmware (ACPI) watches to cool the machine: on a laptop
//! usually near the processor, on many desktops a fixed placeholder.

use std::sync::Arc;

use windows::Win32::System::Performance::{
    PDH_FMT, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW,
};
use windows::core::{PCWSTR, w};

use super::*;

/// The value as the counter stores it, without its display scale.
const PDH_FMT_NOSCALE: u32 = 0x1000;

/// The device thermal zones are listed under.
const ZONES: &str = "Thermal zones";

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

pub struct SensorSampler {
    query: Option<Query>,
}

impl Default for SensorSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl SensorSampler {
    pub fn new() -> Self {
        Self {
            query: Query::open(),
        }
    }

    pub fn sample(&mut self) -> Vec<Sensor> {
        self.query.as_ref().map_or_else(Vec::new, Query::read)
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
    fn reads_the_thermal_zones() {
        for s in SensorSampler::new().sample() {
            println!("{} {:.1} °C", s.label, s.value);
            assert!((-40.0..=150.0).contains(&s.value));
        }
    }
}
