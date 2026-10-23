// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Scroll benchmark: with ANABOLIC_BENCH_SCROLL=<report file> set, the
//! Performance page scrolls itself and per-frame costs are written to the
//! report. ANABOLIC_BENCH_MODE=wheel scrolls with injected mouse wheel notches
//! and the pointer over the page (hover effects, wheel smoothing), otherwise
//! the scroll offset is set directly. ANABOLIC_BENCH_MODE=soak runs until
//! closed: a short scroll burst every minute, one report line per burst, so
//! costs that grow with uptime show up. Inert unless the variable is set.

use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use eframe::egui;

const WARMUP: Duration = Duration::from_secs(4);
const FRAMES: usize = 600;
/// Offset mode: pixels per second, and how far down before turning around.
const SPEED: f32 = 1200.0;
const DEPTH: f32 = 2400.0;
/// Wheel mode: one notch this often, this many down then as many up.
const NOTCH_EVERY: Duration = Duration::from_millis(60);
const NOTCHES: u32 = 30;
/// Soak mode: a burst this long every this many seconds.
const SOAK_EVERY: f32 = 60.0;
const SOAK_BURST: f32 = 3.0;

struct Bench {
    path: std::path::PathBuf,
    wheel: bool,
    soak: bool,
    /// Soak mode: (frame cpu ms, frame interval ms) of the current burst.
    burst: Vec<[f32; 2]>,
    started: Instant,
    notches: u32,
    last_notch: Option<Instant>,
    /// (frame cpu ms, ui ms, frame interval ms, scroll movement px)
    samples: Vec<[f32; 4]>,
    last_offset: Option<f32>,
    moved: f32,
    /// Page height changes while scrolling: (count, largest).
    last_height: Option<f32>,
    height_changes: (u32, f32),
    done: bool,
}

static BENCH: OnceLock<Option<Mutex<Bench>>> = OnceLock::new();

fn bench() -> Option<&'static Mutex<Bench>> {
    BENCH
        .get_or_init(|| {
            std::env::var_os("ANABOLIC_BENCH_SCROLL").map(|path| {
                Mutex::new(Bench {
                    path: path.into(),
                    wheel: std::env::var("ANABOLIC_BENCH_MODE").is_ok_and(|m| m == "wheel"),
                    soak: std::env::var("ANABOLIC_BENCH_MODE").is_ok_and(|m| m == "soak"),
                    burst: Vec::new(),
                    started: Instant::now(),
                    notches: 0,
                    last_notch: None,
                    samples: Vec::with_capacity(FRAMES),
                    last_offset: None,
                    moved: 0.0,
                    last_height: None,
                    height_changes: (0, 0.0),
                    done: false,
                })
            })
        })
        .as_ref()
}

pub fn enabled() -> bool {
    bench().is_some()
}

/// Where the page should be scrolled to this frame (offset mode).
pub fn scroll_offset(ctx: &egui::Context) -> Option<f32> {
    let b = bench()?.lock().ok()?;
    if b.soak {
        let t = b.started.elapsed().as_secs_f32();
        let into = t % SOAK_EVERY;
        if t < SOAK_EVERY || into >= SOAK_BURST {
            ctx.request_repaint_after(Duration::from_secs_f32(SOAK_EVERY - into));
            return None;
        }
        ctx.request_repaint();
        let phase = (into * SPEED) % (2.0 * DEPTH);
        return Some(if phase < DEPTH {
            phase
        } else {
            2.0 * DEPTH - phase
        });
    }
    if b.done || b.wheel {
        return None;
    }
    ctx.request_repaint();
    let t = b.started.elapsed().saturating_sub(WARMUP).as_secs_f32();
    // Down and back up: a triangle wave.
    let phase = (t * SPEED) % (2.0 * DEPTH);
    Some(if phase < DEPTH {
        phase
    } else {
        2.0 * DEPTH - phase
    })
}

/// Wheel mode: the pointer rests over the page and a wheel notch comes every
/// so often, put into the frame's input like the window's own events. No
/// repaint is forced: frames come only when egui itself asks for them, as
/// with a real mouse.
pub fn inject(raw: &mut egui::RawInput) {
    let Some(bench) = bench() else { return };
    let Ok(mut b) = bench.lock() else { return };
    if !b.wheel || b.done || b.started.elapsed() < WARMUP {
        return;
    }
    let screen = raw.screen_rect.unwrap_or(egui::Rect::from_min_size(
        egui::pos2(0.0, 0.0),
        egui::vec2(1400.0, 860.0),
    ));
    raw.events.push(egui::Event::PointerMoved(egui::pos2(
        screen.width() * 0.55,
        screen.height() * 0.6,
    )));
    if b.last_notch.is_none_or(|t| t.elapsed() >= NOTCH_EVERY) {
        b.last_notch = Some(Instant::now());
        let down = (b.notches / NOTCHES).is_multiple_of(2);
        b.notches += 1;
        raw.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, if down { -1.0 } else { 1.0 }),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
    }
}

/// The page's scroll offset and content height after this frame.
pub fn scrolled_to(offset: f32, height: f32) {
    let Some(bench) = bench() else { return };
    let Ok(mut b) = bench.lock() else { return };
    if !b.done
        && b.started.elapsed() >= WARMUP
        && let Some(last) = b.last_height
        && (height - last).abs() > 0.5
    {
        b.height_changes.0 += 1;
        b.height_changes.1 = b.height_changes.1.max((height - last).abs());
    }
    b.last_height = Some(height);
    if let Some(last) = b.last_offset {
        b.moved = (offset - last).abs();
    }
    b.last_offset = Some(offset);
}

/// Records the previous frame's cost; writes the report once there are enough.
pub fn frame(ctx: &egui::Context, cpu: Option<f32>, ui: Duration) {
    let Some(bench) = bench() else { return };
    let Ok(mut b) = bench.lock() else { return };
    if b.soak {
        soak_frame(&mut b, ctx, cpu);
        return;
    }
    if b.done {
        return;
    }
    if b.started.elapsed() < WARMUP {
        if !b.wheel {
            ctx.request_repaint();
        }
        return;
    }
    if b.wheel {
        // A real wheel's next notch wakes the window by itself.
        ctx.request_repaint_after(NOTCH_EVERY);
    } else {
        ctx.request_repaint();
    }
    let interval = ctx.input(|i| i.unstable_dt) * 1000.0;
    let moved = b.moved;
    b.samples.push([
        cpu.unwrap_or(0.0) * 1000.0,
        ui.as_secs_f32() * 1000.0,
        interval,
        moved,
    ]);
    if b.samples.len() < FRAMES {
        return;
    }
    b.done = true;
    let stats = |i: usize| {
        let mut v: Vec<f32> = b.samples.iter().map(|s| s[i]).collect();
        v.sort_by(f32::total_cmp);
        let at = |q: f32| v[((v.len() - 1) as f32 * q) as usize];
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        format!(
            "mean {mean:6.2}  p50 {:6.2}  p95 {:6.2}  p99 {:6.2}  max {:6.2}",
            at(0.5),
            at(0.95),
            at(0.99),
            at(1.0)
        )
    };
    let slow = b.samples.iter().filter(|s| s[2] > 20.0).count();
    let hitches = b
        .samples
        .windows(2)
        .filter(|w| w[0][3] > 0.5 && w[1][3] > 0.5 && w[1][2] > 12.0)
        .count();
    // Movement that stops dead between two moving frames reads as a hitch.
    let stalls = b
        .samples
        .windows(3)
        .filter(|w| w[0][3] > 2.0 && w[1][3] < 0.1 && w[2][3] > 2.0)
        .count();
    let report = format!(
        "mode {}\nframes {}\nframe cpu ms   {}\nui ms          {}\ninterval ms    {}\nmoved px/frame {}\nintervals > 20 ms: {slow}\nscroll stalls: {stalls}\nlate frames while moving: {hitches}\npage height changes: {} (largest {:.0} px)\n",
        if b.wheel { "wheel" } else { "offset" },
        b.samples.len(),
        stats(0),
        stats(1),
        stats(2),
        stats(3),
        b.height_changes.0,
        b.height_changes.1,
    );
    let _ = std::fs::write(&b.path, report);
}

/// Soak mode: collect frames during a burst, append one line when it ends.
fn soak_frame(b: &mut Bench, ctx: &egui::Context, cpu: Option<f32>) {
    let t = b.started.elapsed().as_secs_f32();
    let in_burst = t >= SOAK_EVERY && t % SOAK_EVERY < SOAK_BURST;
    if in_burst {
        let interval = ctx.input(|i| i.unstable_dt) * 1000.0;
        b.burst.push([cpu.unwrap_or(0.0) * 1000.0, interval]);
        return;
    }
    if b.burst.len() < 2 {
        b.burst.clear();
        return;
    }
    // The first frame's interval spans the idle gap before the burst.
    let samples: Vec<[f32; 2]> = b.burst.drain(..).skip(1).collect();
    let pick = |i: usize| {
        let mut v: Vec<f32> = samples.iter().map(|s| s[i]).collect();
        v.sort_by(f32::total_cmp);
        let at = |q: f32| v[((v.len() - 1) as f32 * q) as usize];
        (at(0.5), at(0.95), at(1.0))
    };
    let (c50, c95, cmax) = pick(0);
    let (_, i95, imax) = pick(1);
    let line = format!(
        "{:>4.0} min  frames {:>4}  cpu ms p50 {c50:6.2} p95 {c95:6.2} max {cmax:6.2}  interval ms p95 {i95:6.2} max {imax:7.2}  ws {} MB
",
        t / 60.0,
        samples.len(),
        anabolic_core::hardening::own_working_set() / (1024 * 1024),
    );
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&b.path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}
