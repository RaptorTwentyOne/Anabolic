//! Records UI stalls in %LOCALAPPDATA%\Anabolic\stalls.log, one line each
//! with the page:
//!
//! - a late frame: one that was due at once (the previous frame asked for
//!   it, or a background thread woke the UI) but started late. The wait is
//!   measured from when it was due, so an idle gap that ends with a wake-up
//!   is not a stall.
//! - a slow frame: one that took too long itself (wall time inside eframe:
//!   our update, tessellation and painting).
//!
//! Lines go to the file from a writer thread: the UI never waits on disk.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use eframe::egui;

/// A due frame later than this, or a frame longer than this, is a stall.
const THRESHOLD: f32 = 0.15;
/// Startup and window restores produce long gaps that are not stalls.
const GRACE: Duration = Duration::from_secs(5);
const MAX_LOG: u64 = 1024 * 1024;

#[derive(Default)]
pub struct Stalls {
    pub count: u32,
    pub longest: f32,
    started: Option<Instant>,
    was_minimized: bool,
}

pub fn log_path() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Anabolic");
    Some(dir.join("stalls.log"))
}

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// When a background thread last woke the UI, in nanoseconds since `epoch`.
static LAST_WAKE: AtomicU64 = AtomicU64::new(0);

/// Wakes the UI from another thread, noting when, so the frame it causes is
/// measured from now and not from the end of the idle gap before it.
pub fn wake(ctx: &egui::Context) {
    LAST_WAKE.store(epoch().elapsed().as_nanos() as u64, Ordering::Relaxed);
    ctx.request_repaint();
}

impl Stalls {
    /// Call once per frame, first thing. `prev_frame` is how long the
    /// previous frame took in seconds, as eframe reports it.
    pub fn frame(&mut self, ctx: &egui::Context, prev_frame: Option<f32>, page: &str) {
        let started = *self.started.get_or_insert_with(Instant::now);
        let (stable, unstable, minimized) = ctx.input(|i| {
            (
                i.stable_dt,
                i.unstable_dt,
                i.viewport().minimized.unwrap_or(false),
            )
        });
        let was_minimized = std::mem::replace(&mut self.was_minimized, minimized);
        if minimized || was_minimized || started.elapsed() < GRACE {
            return;
        }
        let prev_frame = prev_frame.unwrap_or(0.0);
        // egui trusts the gap since the previous frame only when that frame
        // asked for an immediate repaint, or a wake-up came meanwhile; then
        // the wait counts from the wake-up if there was one.
        let since_wake = epoch()
            .elapsed()
            .saturating_sub(Duration::from_nanos(LAST_WAKE.load(Ordering::Relaxed)))
            .as_secs_f32();
        // Window events (close, resize, focus) wake egui from inside the event
        // loop at an unknown time, like input: the gap before them was idle.
        let causes = ctx.repaint_causes();
        let window_event = causes.iter().any(|c| {
            [
                "wgpu_integration",
                "epi_integration",
                "egui-winit",
                "egui_winit",
            ]
            .iter()
            .any(|f| c.file.contains(f))
        });
        let due = (stable - unstable).abs() < 1e-4 && !window_event;
        let late = if due { unstable.min(since_wake) } else { 0.0 };
        if late < THRESHOLD && prev_frame < THRESHOLD {
            return;
        }
        self.count += 1;
        self.longest = self.longest.max(late).max(prev_frame);
        let causes: Vec<String> = causes
            .iter()
            .take(3)
            .map(|c| {
                format!(
                    "{}:{}",
                    c.file.rsplit(['/', '\\']).next().unwrap_or(c.file),
                    c.line
                )
            })
            .collect();
        log(format!(
            "{}  late {:>5.0} ms  previous frame {:>6.1} ms  page {page}  woken by {}\n",
            local_time(),
            late * 1000.0,
            prev_frame * 1000.0,
            if causes.is_empty() {
                "input".to_string()
            } else {
                causes.join(", ")
            },
        ));
    }
}

fn log(line: String) {
    static WRITER: OnceLock<Option<Sender<String>>> = OnceLock::new();
    let writer = WRITER.get_or_init(|| {
        let path = log_path()?;
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::Builder::new()
            .name("anabolic-stalls".into())
            .spawn(move || {
                for line in rx {
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG) {
                        let _ = std::fs::rename(&path, path.with_extension("old.log"));
                    }
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                    {
                        let _ = f.write_all(line.as_bytes());
                    }
                }
            })
            .ok()?;
        Some(tx)
    });
    if let Some(tx) = writer {
        let _ = tx.send(line);
    }
}

fn local_time() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}
