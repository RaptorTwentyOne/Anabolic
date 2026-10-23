// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Sensors: every temperature, fan and power reading, grouped by the
//! device it sits on, and how the temperatures shown elsewhere look.

use anabolic_core::sensors::{self, Heat, Sensor, SensorKind};

use super::*;

/// A reading's text colour: plain, yellow when warm, red when hot.
pub(super) fn heat_text(heat: Heat) -> egui::Color32 {
    match heat {
        Heat::Normal => theme::TEXT,
        Heat::Warm => theme::ELEVATED_TEXT,
        Heat::Hot => theme::BAD,
    }
}

/// A temperature graph's colour: its device's, yellow when warm, red
/// when hot.
pub(super) fn heat_line(heat: Heat, normal: egui::Color32) -> egui::Color32 {
    match heat {
        Heat::Normal => normal,
        Heat::Warm => theme::ELEVATED_TEXT,
        Heat::Hot => theme::BAD,
    }
}

/// "67 °C", "1,820 RPM", "4.2 W".
pub(super) fn reading(value: f32, kind: SensorKind) -> String {
    match kind {
        SensorKind::Temperature => format!("{value:.0} °C"),
        SensorKind::Fan => format!("{} RPM", fmt::count(value.max(0.0) as u64)),
        SensorKind::Power if value < 10.0 => format!("{value:.1} W"),
        SensorKind::Power => format!("{value:.0} W"),
    }
}

/// A sensor's value, coloured by its heat.
pub(super) fn reading_text(sensor: &Sensor) -> RichText {
    let text = RichText::new(reading(sensor.value, sensor.kind));
    match sensor.kind {
        SensorKind::Temperature => text.color(heat_text(sensor.heat())),
        _ => text,
    }
}

/// A temperature's history, scaled to the hardware's limit so the graph
/// shows how much room is left.
pub(super) fn temperature_graph(
    ui: &mut Ui,
    snap: &Snapshot,
    ring: &Ring,
    sensor: &Sensor,
    color: egui::Color32,
    height: f32,
) {
    let limit = sensor.critical.or(sensor.high).unwrap_or(100.0);
    big_graph(
        ui,
        Graph::new(Some(limit.max(ring.max()))).series(ring, heat_line(sensor.heat(), color), true),
        height,
        ring,
        snap,
        |m| format!("{m:.0} °C"),
    );
}

/// The nav card's line: the processor's temperature, else the hottest.
pub(super) fn summary(list: &[Sensor]) -> String {
    let count = match list.len() {
        0 => return "None reported".into(),
        1 => "1 sensor".to_string(),
        n => format!("{n} sensors"),
    };
    if let Some(cpu) = sensors::cpu_sensor(list) {
        return format!("CPU {:.0} °C  ·  {count}", cpu.value);
    }
    match list
        .iter()
        .filter(|s| s.kind == SensorKind::Temperature)
        .max_by(|a, b| a.value.total_cmp(&b.value))
    {
        Some(hottest) => format!("Hottest {:.0} °C  ·  {count}", hottest.value),
        None => count,
    }
}

/// Where the readings come from, and on Windows why the processor's
/// temperature is missing: what stands in for it depends on the PC.
#[cfg(windows)]
fn note(list: &[Sensor]) -> String {
    let zones = list
        .iter()
        .any(|s| &*s.device == anabolic_core::sensors::ZONES);
    let power = list
        .iter()
        .any(|s| &*s.device == sensors::CPU && s.kind == SensorKind::Power);
    let mut note = String::from(
        "Windows lets only drivers read the processor's own temperature sensors, and Anabolic \
         installs none. ",
    );
    note.push_str(if zones {
        "The firmware's thermal zones stand in for them (on many desktops a fixed \
         placeholder). "
    } else {
        "This PC's firmware has no thermal zones to stand in for them. "
    });
    if power {
        note.push_str("The processor's power comes from its energy meter. ");
    }
    note.push_str("GPUs and disks report their own sensors.");
    note
}

#[cfg(not(windows))]
fn note(_: &[Sensor]) -> String {
    "From the kernel's hardware monitors and the firmware's thermal zones. A sensor on a \
     device that sleeps (a laptop's second GPU) is left out rather than woken."
        .into()
}

const NONE: &str = if cfg!(windows) {
    "Nothing here reports a temperature, fan or power: no thermal zones or energy meters, \
     and no GPU or disk that does."
} else {
    "No sensor reports here. The driver for the board's sensor chip may not be loaded: \
     sensors-detect, from lm_sensors, finds it."
};

/// The width of a value column.
const VALUE: f32 = 110.0;

pub(super) fn sensors(ui: &mut Ui, snap: &Snapshot) {
    let s = &snap.system;
    let h = &snap.history;
    title(
        ui,
        "Sensors",
        &match s.sensors.len() {
            1 => "1 sensor".to_string(),
            n => format!("{n} sensors"),
        },
    );
    ui.label(
        RichText::new(note(&s.sensors))
            .small()
            .color(theme::TEXT_DIM),
    );
    ui.add_space(6.0);
    if let Some(cpu) = sensors::cpu_sensor(&s.sensors)
        && !h.cpu_temperature.is_empty()
    {
        temperature_graph(ui, snap, &h.cpu_temperature, cpu, theme::CPU, 140.0);
        legend(
            ui,
            &[(heat_line(cpu.heat(), theme::CPU), "CPU temperature")],
        );
    }
    if s.sensors.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new(NONE).color(theme::TEXT_WEAK));
        return;
    }

    // By device, in the order they come.
    let mut groups: Vec<(&str, Vec<&Sensor>)> = Vec::new();
    for sensor in &s.sensors {
        match groups.iter_mut().find(|(d, _)| *d == &*sensor.device) {
            Some((_, list)) => list.push(sensor),
            None => groups.push((&sensor.device, vec![sensor])),
        }
    }
    let limit = |v: Option<f32>, kind: SensorKind| match v {
        Some(v) if kind == SensorKind::Temperature => reading(v, kind),
        _ => "—".into(),
    };
    // One width for every device's table, so their values line up.
    let name = (ui.available_width() - 3.0 * VALUE).clamp(220.0, 520.0);
    for (device, list) in groups {
        section(ui, device);
        // The limits only where the device reports any: a column of
        // dashes says nothing.
        let limits = list.iter().any(|s| {
            s.kind == SensorKind::Temperature && (s.high.is_some() || s.critical.is_some())
        });
        let mut columns = vec![("Sensor", name, Align::Min), ("Now", VALUE, Align::Max)];
        if limits {
            columns.extend([("High", VALUE, Align::Max), ("Critical", VALUE, Align::Max)]);
        }
        widgets::header(ui, &columns);
        for (i, sensor) in list.into_iter().enumerate() {
            widgets::row(ui, i, |ui| {
                widgets::text(ui, name, Align::Min, &*sensor.label);
                widgets::text(ui, VALUE, Align::Max, reading_text(sensor));
                if limits {
                    for mark in [sensor.high, sensor.critical] {
                        widgets::text(
                            ui,
                            VALUE,
                            Align::Max,
                            RichText::new(limit(mark, sensor.kind)).color(theme::TEXT_WEAK),
                        );
                    }
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn readings_read_well() {
        assert_eq!(reading(67.4, SensorKind::Temperature), "67 °C");
        assert_eq!(reading(3.36, SensorKind::Power), "3.4 W");
        assert_eq!(reading(42.0, SensorKind::Power), "42 W");
        let cpu = Sensor {
            device: Arc::from(sensors::CPU),
            label: Arc::from("Package"),
            kind: SensorKind::Temperature,
            value: 61.0,
            high: Some(100.0),
            critical: Some(100.0),
        };
        assert_eq!(summary(&[]), "None reported");
        assert_eq!(summary(&[cpu]), "CPU 61 °C  ·  1 sensor");
    }
}
