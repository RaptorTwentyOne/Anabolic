// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Hardware facts below the graphs: the processor, the memory modules and
//! a network adapter.

use super::*;

pub(super) fn adapter(ui: &mut Ui, snap: &Snapshot, luid: u64) {
    let Some(a) = snap.system.net.adapters.iter().find(|a| a.luid == luid) else {
        ui.label(RichText::new("This adapter is down or gone.").color(theme::TEXT_WEAK));
        return;
    };
    title(ui, &a.name, &a.description);
    if let Some(ah) = snap.history.adapter(luid) {
        big_graph(
            ui,
            Graph::new(None).series(&ah.rx, theme::NET_RX, true).series(
                &ah.tx,
                theme::NET_TX,
                true,
            ),
            200.0,
            &ah.rx,
            snap,
            |v| format!("{}/s", fmt::bytes(v as u64)),
        );
        legend(ui, &[(theme::NET_RX, "Receive"), (theme::NET_TX, "Send")]);
    }
    ui.add_space(10.0);
    let utilization = if a.link_speed > 0 {
        (a.rx_rate + a.tx_rate) * 8.0 * 100.0 / a.link_speed as f64
    } else {
        0.0
    };
    stats(
        ui,
        &[
            ("Receive", fmt::rate_always(a.rx_rate)),
            ("Send", fmt::rate_always(a.tx_rate)),
            ("Link speed", link_speed(a.link_speed)),
            ("Link use", format!("{utilization:.2} %")),
            ("Received", fmt::bytes(a.rx_total)),
            ("Sent", fmt::bytes(a.tx_total)),
        ],
    );
}

/// Key/value rows with fixed columns.
pub(super) fn facts(ui: &mut Ui, items: &[(&str, String)]) {
    for (i, (key, value)) in items.iter().enumerate() {
        widgets::row(ui, i, |ui| {
            widgets::text(
                ui,
                190.0,
                Align::Min,
                RichText::new(*key).color(theme::TEXT_WEAK),
            );
            widgets::text(ui, 520.0, Align::Min, value.clone());
        });
    }
}

pub(super) fn section(ui: &mut Ui, name: &str) {
    ui.add_space(14.0);
    ui.label(widgets::strong(name, theme::SUBTITLE));
    ui.add_space(4.0);
}

pub(super) fn cpu_info(ui: &mut Ui, snap: &Snapshot, hw: &HardwareInfo) {
    let c = &hw.cpu;
    title(ui, "Processor info", &c.brand);
    let smt = if c.threads > c.cores {
        "SMT on"
    } else {
        "SMT off"
    };
    facts(
        ui,
        &[
            ("Name", c.brand.clone()),
            ("Vendor", c.vendor.clone()),
            (
                "Family / model / stepping",
                format!(
                    "{} ({:X}h) / {} ({:X}h) / {}",
                    c.family, c.family, c.model, c.model, c.stepping
                ),
            ),
            (
                "Socket",
                format!(
                    "{}  ·  {} socket(s)",
                    if c.socket.is_empty() { "?" } else { &c.socket },
                    c.sockets.max(1)
                ),
            ),
            (
                "Cores / threads",
                format!("{} / {}  ({smt})", c.cores, c.threads),
            ),
            ("NUMA nodes", c.numa_nodes.to_string()),
            ("Base clock", ghz(c.base_mhz as f32)),
            ("Max clock (firmware)", ghz(c.max_mhz as f32)),
            (
                "Effective clock now",
                format!("{}  (average of active cores)", ghz(snap.system.cpu.mhz)),
            ),
            (
                "Bus clock",
                if c.bus_mhz > 0 {
                    format!("{} MHz", c.bus_mhz)
                } else {
                    "—".into()
                },
            ),
            (
                "Voltage (firmware)",
                c.voltage.map_or("—".into(), |v| format!("{v:.2} V")),
            ),
            (
                "Microcode",
                c.microcode.clone().unwrap_or_else(|| "—".into()),
            ),
            (
                "Virtualization",
                if c.virtualization_enabled {
                    "Enabled in firmware".into()
                } else {
                    "Disabled".into()
                },
            ),
            (
                "Hypervisor",
                if c.hypervisor_present && cfg!(windows) {
                    "Windows runs on a hypervisor (VBS / Hyper-V)".into()
                } else if c.hypervisor_present {
                    "Linux runs in a virtual machine".into()
                } else {
                    "None".into()
                },
            ),
        ],
    );

    section(ui, "Caches");
    let caches: Vec<(&str, String)> = c
        .caches
        .iter()
        .map(|(name, size, count)| {
            (
                name.as_str(),
                format!("{} × {}", count, fmt::bytes(*size as u64)),
            )
        })
        .collect();
    facts(ui, &caches);

    section(ui, "Instruction set extensions");
    ui.horizontal_wrapped(|ui| {
        for f in &c.features {
            egui::Frame::new()
                .fill(theme::RAISED)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .corner_radius(3)
                .inner_margin(egui::Margin::symmetric(6, 2))
                .show(ui, |ui| ui.label(RichText::new(*f).small()));
        }
    });

    section(ui, "Live clocks");
    let cores = &snap.system.cores;
    let max = cores
        .iter()
        .map(|c| c.mhz)
        .fold(c.max_mhz as f32, f32::max)
        .max(1.0);
    for (row, chunk) in cores.chunks(8).enumerate() {
        ui.horizontal(|ui| {
            for (i, core) in chunk.iter().enumerate() {
                let cpu = row * 8 + i;
                let domain = hw
                    .topology
                    .l3_domain(cpu)
                    .map_or(String::new(), |d| format!("CCD {d} "));
                widgets::cell(ui, 150.0, Align::Min, |ui| {
                    ui.label(
                        RichText::new(format!("{domain}CPU {cpu}"))
                            .small()
                            .color(theme::TEXT_DIM),
                    );
                    let color = if core.mhz <= 0.0 {
                        theme::TEXT_DIM
                    } else {
                        // Brighter for the faster cores, in the CPU hue.
                        theme::TEXT_WEAK.lerp_to_gamma(theme::CPU, core.mhz / max)
                    };
                    ui.label(RichText::new(ghz(core.mhz)).color(color));
                });
            }
        });
    }

    section(ui, "Board & firmware");
    let b = &hw.board;
    facts(
        ui,
        &[
            ("Motherboard", format!("{} {}", b.manufacturer, b.product)),
            (
                "BIOS",
                format!("{} {}  ·  {}", b.bios_vendor, b.bios_version, b.bios_date),
            ),
        ],
    );
}

pub(super) fn memory_info(ui: &mut Ui, snap: &Snapshot, hw: &HardwareInfo) {
    let m = &hw.memory;
    let usable = snap.system.memory.total;
    title(
        ui,
        "Memory modules",
        &format!("{} installed", fmt::bytes(m.installed)),
    );
    let rated = m.modules.iter().map(|x| x.speed).max().unwrap_or(0);
    let configured = m
        .modules
        .iter()
        .map(|x| x.configured_speed)
        .filter(|&v| v > 0)
        .min()
        .unwrap_or(0);
    let kind = m.modules.first().map_or("", |x| x.kind);
    facts(
        ui,
        &[
            ("Installed", fmt::bytes(m.installed)),
            (
                if cfg!(windows) {
                    "Usable by Windows"
                } else {
                    "Usable by Linux"
                },
                fmt::bytes(usable),
            ),
            (
                "Hardware reserved",
                fmt::bytes(m.installed.saturating_sub(usable)),
            ),
            ("Slots used", format!("{} of {}", m.modules.len(), m.slots)),
            ("Type", kind.to_string()),
            (
                "Running at",
                if configured > 0 {
                    format!("{configured} MT/s")
                } else {
                    "—".into()
                },
            ),
            (
                "Modules rated",
                if rated > 0 {
                    format!("{rated} MT/s (SPD/JEDEC)")
                } else {
                    "—".into()
                },
            ),
            (
                "Maximum capacity",
                if m.max_capacity > 0 {
                    fmt::bytes(m.max_capacity)
                } else {
                    "—".into()
                },
            ),
            ("Error correction", m.error_correction.to_string()),
        ],
    );
    if configured > 0 && rated > 0 && configured < rated {
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "The memory runs at {configured} MT/s, below the {rated} MT/s the modules report. Check the memory \
                 profile (XMP / EXPO) in the BIOS; boards with every slot filled often need a lower speed to be stable."
            ))
            .color(theme::ELEVATED_TEXT),
        );
    }

    section(ui, "Modules");
    let columns = [
        ("Slot", 190.0, Align::Min),
        ("Size", 70.0, Align::Max),
        ("Type", 90.0, Align::Min),
        ("Rated", 80.0, Align::Max),
        ("Running", 80.0, Align::Max),
        ("Rank", 50.0, Align::Max),
        ("Voltage", 70.0, Align::Max),
        ("Manufacturer", 120.0, Align::Min),
        ("Part number", 200.0, Align::Min),
    ];
    widgets::header(ui, &columns);
    for (i, x) in m.modules.iter().enumerate() {
        widgets::row(ui, i, |ui| {
            let slot = if x.bank.is_empty() {
                x.locator.clone()
            } else {
                format!("{}  ·  {}", x.locator, x.bank)
            };
            widgets::text(ui, columns[0].1, Align::Min, slot);
            widgets::text(ui, columns[1].1, Align::Max, fmt::bytes(x.size));
            widgets::text(
                ui,
                columns[2].1,
                Align::Min,
                format!("{} {}", x.kind, x.form_factor),
            );
            widgets::text(ui, columns[3].1, Align::Max, format!("{}", x.speed));
            widgets::text(
                ui,
                columns[4].1,
                Align::Max,
                format!("{}", x.configured_speed),
            );
            widgets::text(
                ui,
                columns[5].1,
                Align::Max,
                if x.rank > 0 {
                    x.rank.to_string()
                } else {
                    String::new()
                },
            );
            widgets::text(
                ui,
                columns[6].1,
                Align::Max,
                if x.voltage_mv > 0 {
                    format!("{:.2} V", x.voltage_mv as f32 / 1000.0)
                } else {
                    String::new()
                },
            );
            widgets::text(ui, columns[7].1, Align::Min, x.manufacturer.clone());
            widgets::text(
                ui,
                columns[8].1,
                Align::Min,
                RichText::new(x.part_number.clone()).monospace(),
            );
        });
    }

    section(ui, "Page files");
    let columns = [
        ("File", 260.0, Align::Min),
        ("Size", 100.0, Align::Max),
        ("In use", 100.0, Align::Max),
        ("Peak", 100.0, Align::Max),
    ];
    widgets::header(ui, &columns);
    for (i, pf) in snap.system.page_files.iter().enumerate() {
        widgets::row(ui, i, |ui| {
            widgets::text(ui, columns[0].1, Align::Min, pf.path.clone());
            widgets::text(ui, columns[1].1, Align::Max, fmt::bytes(pf.size));
            widgets::text(ui, columns[2].1, Align::Max, fmt::bytes(pf.used));
            widgets::text(
                ui,
                columns[3].1,
                Align::Max,
                RichText::new(fmt::bytes(pf.peak)).color(theme::TEXT_WEAK),
            );
        });
    }
    if snap.system.page_files.is_empty() {
        ui.label(RichText::new("No page file.").color(theme::TEXT_DIM));
    }
}
