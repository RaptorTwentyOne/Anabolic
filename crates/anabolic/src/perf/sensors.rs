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

const NOTE: &str = if cfg!(windows) {
    "Windows lets only drivers read the processor's own sensors, and Anabolic installs \
     none: what stands in for them are the firmware's thermal zones, which it cools the \
     machine by (on many desktops a fixed placeholder). GPUs and disks report their own."
} else {
    "From the kernel's hardware monitors and the firmware's thermal zones. A sensor on a \
     device that sleeps (a laptop's second GPU) is left out rather than woken."
};

const NONE: &str = if cfg!(windows) {
    "Nothing here reports a temperature: no thermal zones, and no GPU or disk that does."
} else {
    "No sensor reports here. The driver for the board's sensor chip may not be loaded: \
     sensors-detect, from lm_sensors, finds it."
};

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
    ui.label(RichText::new(NOTE).small().color(theme::TEXT_DIM));
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
    for (device, list) in groups {
        section(ui, device);
        widgets::header(
            ui,
            &[
                ("Sensor", 300.0, Align::Min),
                ("Now", 100.0, Align::Max),
                ("High", 100.0, Align::Max),
                ("Critical", 100.0, Align::Max),
            ],
        );
        for (i, sensor) in list.into_iter().enumerate() {
            widgets::row(ui, i, |ui| {
                widgets::text(ui, 300.0, Align::Min, &*sensor.label);
                widgets::text(ui, 100.0, Align::Max, reading_text(sensor));
                widgets::text(
                    ui,
                    100.0,
                    Align::Max,
                    RichText::new(limit(sensor.high, sensor.kind)).color(theme::TEXT_WEAK),
                );
                widgets::text(
                    ui,
                    100.0,
                    Align::Max,
                    RichText::new(limit(sensor.critical, sensor.kind)).color(theme::TEXT_WEAK),
                );
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
