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
mod history_page;
mod icons;
mod perf;
mod proc_table;
mod settings;
#[cfg(debug_assertions)]
mod shot;
mod stalls;
mod theme;
mod tree;
mod widgets;

use anabolic_core::instance::{self, Claim};
use eframe::egui;

fn main() -> eframe::Result {
    attach_parent_console();
    // "Restart as admin" starts us while the instance it replaces closes.
    let args: Vec<String> = std::env::args().collect();
    if let Some(pid) = args
        .iter()
        .position(|a| a == "--after")
        .and_then(|i| args.get(i + 1)?.parse().ok())
    {
        instance::await_exit(pid, std::time::Duration::from_secs(10));
    }
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
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: wgpu_options(),
        // Test instances keep their state elsewhere, away from the user's.
        // (eframe wants the file here, not the folder its docs mention.)
        persistence_path: std::env::var_os("ANABOLIC_DATA_DIR")
            .map(|dir| std::path::Path::new(&dir).join("app.ron")),
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
                    stalls::wake(&ctx);
                });
            }
            Ok(Box::new(app::App::new(cc, hardening, elevated)))
        }),
    )
}

/// Direct3D 12, presented with the flip model. OpenGL on Windows goes
/// through DWM's legacy blit path, and while such a window is in the
/// foreground DWM walks the session's named objects on every frame it
/// composes (dwmcore!OpenDxBltEvent). On a busy session that keeps DWM's
/// composition thread at a full core and the whole desktop stops keeping up.
fn wgpu_options() -> eframe::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuSetup, WgpuSetupCreateNew};
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
    // Debug builds are what the user runs every day: no D3D12 debug layer
    // (WGPU_VALIDATION=1 brings it back).
    setup.instance_descriptor.flags = eframe::wgpu::InstanceFlags::empty().with_env();
    setup.native_adapter_selector = Some(std::sync::Arc::new(pick_adapter));
    // The default sizes memory blocks for games: 128 MB on the GPU, 64 MB in
    // system memory, some 260 MB of private bytes for a few MB of egui.
    let device = setup.device_descriptor.clone();
    setup.device_descriptor = std::sync::Arc::new(move |adapter| eframe::wgpu::DeviceDescriptor {
        memory_hints: eframe::wgpu::MemoryHints::MemoryUsage,
        ..device(adapter)
    });
    eframe::WgpuConfiguration {
        surface: eframe::SurfaceConfig::LOW_LATENCY,
        wgpu_setup: WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

/// Draws on the GPU the monitors hang off, the primary one's first: a frame
/// rendered elsewhere has to be copied across to be shown. Windows' own
/// order is no guide, it can put an idle integrated GPU first. The software
/// rasterizer only when there is no hardware, as in a virtual machine.
fn pick_adapter(
    adapters: &[eframe::wgpu::Adapter],
    surface: Option<&eframe::wgpu::Surface<'_>>,
) -> Result<eframe::wgpu::Adapter, String> {
    use eframe::wgpu;
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
    let primary = unsafe { MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) };
    // 2: shows the primary monitor, 1: shows another, 0: shows nothing.
    let displays = |adapter: &wgpu::Adapter| {
        let Some(hal) = (unsafe { adapter.as_hal::<wgpu::hal::api::Dx12>() }) else {
            return 0;
        };
        let raw = hal.raw_adapter();
        let mut best = 0;
        for i in 0.. {
            let Ok(output) = (unsafe { raw.EnumOutputs(i) }) else {
                break;
            };
            if let Ok(desc) = unsafe { output.GetDesc() }
                && desc.AttachedToDesktop.as_bool()
            {
                best = best.max(if desc.Monitor == primary { 2 } else { 1 });
            }
        }
        best
    };
    let presents = |a: &&wgpu::Adapter| surface.is_none_or(|s| a.is_surface_supported(s));
    let hardware = |a: &&wgpu::Adapter| a.get_info().device_type != wgpu::DeviceType::Cpu;
    adapters
        .iter()
        .filter(presents)
        .filter(hardware)
        .enumerate()
        // Highest score; the earliest of equals.
        .max_by_key(|&(i, a)| (displays(a), std::cmp::Reverse(i)))
        .map(|(_, a)| a)
        .or_else(|| adapters.iter().find(presents))
        .cloned()
        .ok_or_else(|| "no Direct3D 12 adapter can show the window".to_string())
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

/// Where Anabolic keeps its own files (stall log, history): the user's
/// %LOCALAPPDATA%\Anabolic, or ANABOLIC_DATA_DIR for a test instance.
pub fn data_dir() -> Option<std::path::PathBuf> {
    match std::env::var_os("ANABOLIC_DATA_DIR") {
        Some(dir) => Some(dir.into()),
        None => Some(std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Anabolic")),
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

/// The Steroid Core mark on its tile, from assets/logo (make.mjs).
fn app_icon() -> egui::IconData {
    egui::IconData {
        rgba: include_bytes!("../assets/logo/icon-64.rgba").to_vec(),
        width: 64,
        height: 64,
    }
}
