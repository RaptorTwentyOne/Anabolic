//! Debug builds only: with ANABOLIC_SCREENSHOT=<file.bmp> set, the app saves
//! a picture of its own window a few seconds after starting. Capturing the
//! window from outside needs the desktop to be composited, which it is not
//! while the display sleeps; this renders it from the inside.
//! ANABOLIC_SCREENSHOT_ZOOM=<factor> fits more of a long page in.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage};

static STARTED: OnceLock<Instant> = OnceLock::new();
static REQUESTED: AtomicBool = AtomicBool::new(false);
/// ANABOLIC_SCREENSHOT_AFTER=<seconds> waits longer, for pages that fill slowly.
fn after() -> Duration {
    std::env::var("ANABOLIC_SCREENSHOT_AFTER")
        .ok()
        .and_then(|s| s.parse().ok())
        .map_or(Duration::from_secs(5), Duration::from_secs_f32)
}

pub fn frame(ctx: &egui::Context) {
    let Some(path) = std::env::var_os("ANABOLIC_SCREENSHOT") else {
        return;
    };
    let started = *STARTED.get_or_init(|| {
        if let Some(zoom) = std::env::var("ANABOLIC_SCREENSHOT_ZOOM")
            .ok()
            .and_then(|z| z.parse().ok())
        {
            ctx.set_zoom_factor(zoom);
        }
        Instant::now()
    });
    if !REQUESTED.load(Ordering::Relaxed) {
        if started.elapsed() >= after() {
            REQUESTED.store(true, Ordering::Relaxed);
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        return;
    }
    ctx.input(|i| {
        for event in &i.raw.events {
            if let egui::Event::Screenshot { image, .. } = event
                && let Err(e) = std::fs::write(&path, bmp(image))
            {
                eprintln!("anabolic: screenshot: {e}");
            }
        }
    });
}

/// A 24-bit bottom-up BMP.
fn bmp(image: &ColorImage) -> Vec<u8> {
    let [w, h] = image.size;
    let stride = (w * 3).div_ceil(4) * 4;
    let size = 54 + stride * h;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for y in (0..h).rev() {
        let row = &image.pixels[y * w..(y + 1) * w];
        let start = out.len();
        for p in row {
            out.extend_from_slice(&[p.b(), p.g(), p.r()]);
        }
        out.resize(start + stride, 0);
    }
    out
}
