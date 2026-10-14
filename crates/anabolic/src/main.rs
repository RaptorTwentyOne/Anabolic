//! Anabolic: a Windows task manager that stays responsive when the machine
//! does not.

// Debug builds too: started by the Ctrl+Shift+Esc task, a console program
// would bring a console window along. Run from a terminal, the parent's
// console is attached below.
#![windows_subsystem = "windows"]

mod app;
mod bench;
mod details;
mod dialogs;
mod fmt;
mod graph;
mod icons;
mod perf;
mod proc_table;
mod settings;
#[cfg(debug_assertions)]
mod shot;
mod theme;
mod tree;
mod widgets;

use anabolic_core::instance::{self, Claim};
use eframe::egui;

fn main() -> eframe::Result {
    attach_parent_console();
    // One Anabolic per session: a second start brings the first to the front.
    // `--new` opts out (and leaves activation requests to someone else).
    let activation = if std::env::args().any(|a| a == "--new") {
        None
    } else {
        match instance::claim() {
            Claim::Running => return Ok(()),
            Claim::First(event) => Some(event),
            Claim::Unavailable => None,
        }
    };
    // Before anything else: priority, EcoQoS opt-out, privileges.
    let hardening = anabolic_core::hardening::harden_process();
    let elevated = anabolic_core::hardening::is_elevated();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(if elevated {
                "Anabolic (Administrator)"
            } else {
                "Anabolic"
            })
            .with_inner_size([1400.0, 860.0])
            .with_min_inner_size([760.0, 420.0])
            .with_icon(app_icon())
            // Benchmarks and self-screenshots run beside someone's work: do
            // not take the focus from them.
            .with_active(!(bench::enabled() || std::env::var_os("ANABOLIC_SCREENSHOT").is_some())),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Anabolic",
        options,
        Box::new(move |cc| {
            if let Some(event) = activation {
                let ctx = cc.egui_ctx.clone();
                instance::serve(event, move || {
                    activated();
                    ctx.request_repaint();
                });
            }
            Ok(Box::new(app::App::new(cc, hardening, elevated)))
        }),
    )
}

/// Another instance or the Ctrl+Shift+Esc launcher asked for us. Tests set
/// ANABOLIC_ACTIVATE_LOG to see that happen without the window taking the
/// foreground from whoever is using the machine.
fn activated() {
    match std::env::var_os("ANABOLIC_ACTIVATE_LOG") {
        Some(path) => {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(f, "activated");
            }
        }
        None => {
            instance::bring_to_front();
        }
    }
}

/// A GUI program has no console of its own. Started from one (cargo run)
/// with nowhere else for its output to go, it writes to the parent's.
fn attach_parent_console() {
    use windows::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE,
    };
    let redirected =
        unsafe { GetStdHandle(STD_ERROR_HANDLE) }.is_ok_and(|h| !h.is_invalid() && !h.0.is_null());
    if !redirected {
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// A heartbeat on a hot rounded square, rasterized with anti-aliasing.
fn app_icon() -> egui::IconData {
    const N: usize = 64;
    let pulse = [
        (7.0, 35.0),
        (21.0, 35.0),
        (27.0, 17.0),
        (36.0, 48.0),
        (42.0, 29.0),
        (57.0, 29.0),
    ];
    let mut rgba = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // Rounded square coverage.
            let r = 14.0;
            let dx = (px - 32.0).abs() - (32.0 - r - 1.0);
            let dy = (py - 32.0).abs() - (32.0 - r - 1.0);
            let outside =
                (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0) - r;
            let bg = (0.5 - outside).clamp(0.0, 1.0);
            // Distance to the pulse polyline.
            let d = pulse
                .windows(2)
                .map(|w| segment_distance((px, py), w[0], w[1]))
                .fold(f32::MAX, f32::min);
            let line = (3.4 - d).clamp(0.0, 1.0);
            let t = py / N as f32;
            let base = [255.0, 106.0 - 50.0 * t, 61.0 - 20.0 * t];
            let i = (y * N + x) * 4;
            for c in 0..3 {
                rgba[i + c] = (base[c] * (1.0 - line) + 255.0 * line) as u8;
            }
            rgba[i + 3] = (bg * 255.0) as u8;
        }
    }
    egui::IconData {
        rgba,
        width: N as u32,
        height: N as u32,
    }
}

fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / (abx * abx + aby * aby)).clamp(0.0, 1.0);
    let (cx, cy) = (a.0 + t * abx, a.1 + t * aby);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}
