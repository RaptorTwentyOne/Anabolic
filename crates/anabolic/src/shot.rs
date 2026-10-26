// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

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
    ctx.input(|i| {
        for event in &i.raw.events {
            if let egui::Event::Screenshot {
                image, user_data, ..
            } = event
            {
                // One a script asked for: its number beside the name.
                let path = match user_data
                    .data
                    .as_ref()
                    .and_then(|d| d.downcast_ref::<usize>())
                {
                    Some(n) => {
                        let path = std::path::Path::new(&path);
                        path.with_file_name(format!(
                            "{}-{n}.bmp",
                            path.file_stem().unwrap_or_default().to_string_lossy()
                        ))
                    }
                    None => path.clone().into(),
                };
                if let Err(e) = std::fs::write(&path, bmp(image)) {
                    eprintln!("anabolic: screenshot: {e}");
                }
            }
        }
    });
    if !REQUESTED.load(Ordering::Relaxed) {
        if started.elapsed() >= after() {
            REQUESTED.store(true, Ordering::Relaxed);
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

/// A step of ANABOLIC_SCRIPT.
struct Step {
    at: f32,
    what: String,
    pos: egui::Pos2,
}

/// ANABOLIC_SCRIPT=<steps>: pointer input played into the window as the
/// mouse would give it, to try what takes a click or a drag. The steps,
/// separated by ';', are `seconds:what:x,y`; `what` is `move`, `down` or `up`
/// (the left button), `right` (a right click) or `shot` (a picture saved as
/// `<ANABOLIC_SCREENSHOT's name>-<n>.bmp`).
pub fn script(ctx: &egui::Context, raw: &mut egui::RawInput) {
    static STEPS: OnceLock<std::sync::Mutex<(Instant, Vec<Step>, usize)>> = OnceLock::new();
    let steps = STEPS.get_or_init(|| {
        let steps = std::env::var("ANABOLIC_SCRIPT")
            .unwrap_or_default()
            .split(';')
            .filter_map(|step| {
                let mut parts = step.trim().splitn(3, ':');
                let at = parts.next()?.parse().ok()?;
                let what = parts.next()?.to_string();
                let (x, y) = parts.next().unwrap_or("0,0").split_once(',')?;
                Some(Step {
                    at,
                    what,
                    pos: egui::pos2(x.trim().parse().ok()?, y.trim().parse().ok()?),
                })
            })
            .collect();
        std::sync::Mutex::new((Instant::now(), steps, 0))
    });
    let Ok(mut state) = steps.lock() else { return };
    let (started, steps, next) = &mut *state;
    let now = started.elapsed().as_secs_f32();
    while let Some(step) = steps.get(*next).filter(|s| s.at <= now) {
        let button = |pressed, button| egui::Event::PointerButton {
            pos: step.pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        match step.what.as_str() {
            "move" => raw.events.push(egui::Event::PointerMoved(step.pos)),
            "down" => raw.events.push(button(true, egui::PointerButton::Primary)),
            "up" => raw.events.push(button(false, egui::PointerButton::Primary)),
            "right" => {
                raw.events.push(egui::Event::PointerMoved(step.pos));
                raw.events
                    .push(button(true, egui::PointerButton::Secondary));
                raw.events
                    .push(button(false, egui::PointerButton::Secondary));
            }
            "shot" => ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                egui::UserData::new(*next),
            )),
            _ => {}
        }
        *next += 1;
    }
    if let Some(step) = steps.get(*next) {
        ctx.request_repaint_after(Duration::from_secs_f32((step.at - now).max(0.0)));
    }
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
