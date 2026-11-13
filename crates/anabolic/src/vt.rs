// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! VirusTotal lookups for the window: a file's hash asked about once,
//! one lookup every 15 seconds (a public key's four a minute), the
//! answers kept for the session. Only with the user's API key; without
//! one, the file's page opens in the browser instead. Panes read the
//! answers from here; the app queues the questions.

use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Duration;

use anabolic_core::virustotal::{self, Report};

/// A public key may ask four times a minute.
const SPACING: Duration = Duration::from_secs(15);

#[derive(Default)]
struct State {
    key: Option<String>,
    loaded: bool,
    /// By file path, the hash once computed.
    hashes: HashMap<String, String>,
    /// By hash.
    reports: HashMap<String, Report>,
    /// Paths or hashes waiting.
    queue: VecDeque<String>,
    working: bool,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Mutex::default);
static CTX: OnceLock<eframe::egui::Context> = OnceLock::new();

/// The window to repaint when an answer comes; set once.
pub fn init(ctx: &eframe::egui::Context) {
    let _ = CTX.set(ctx.clone());
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if !s.loaded {
        s.loaded = true;
        s.key = crate::data_dir().and_then(|d| virustotal::load_key(&d));
    }
    f(&mut s)
}

pub fn has_key() -> bool {
    with(|s| s.key.is_some())
}

pub fn set_key(key: &str) -> Result<(), String> {
    let dir = crate::data_dir().ok_or("Anabolic has no data folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    virustotal::save_key(&dir, key)?;
    with(|s| s.key = Some(key.trim().to_string()));
    Ok(())
}

pub fn forget_key() {
    if let Some(dir) = crate::data_dir() {
        virustotal::forget_key(&dir);
    }
    with(|s| {
        s.key = None;
        s.queue.clear();
    });
}

/// What VirusTotal said about a file, by its path or hash.
pub fn report(path_or_hash: &str) -> Option<Report> {
    with(|s| {
        let hash = s
            .hashes
            .get(path_or_hash)
            .cloned()
            .unwrap_or_else(|| path_or_hash.to_lowercase());
        s.reports.get(&hash).cloned()
    })
}

/// Waiting to be asked.
pub fn queued() -> usize {
    with(|s| s.queue.len() + s.working as usize)
}

/// Asks about files by path or hash, unless already asked.
pub fn ask(items: impl IntoIterator<Item = String>) {
    let start = with(|s| {
        if s.key.is_none() {
            return false;
        }
        for item in items {
            let hash = s.hashes.get(&item).cloned().unwrap_or_else(|| item.clone());
            if !s.reports.contains_key(&hash) && !s.queue.contains(&item) {
                s.queue.push_back(item);
            }
        }
        let start = !s.working && !s.queue.is_empty();
        s.working |= start;
        start
    });
    if start {
        let _ = std::thread::Builder::new()
            .name("virustotal".into())
            .spawn(work);
    }
}

fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn work() {
    loop {
        let Some((item, key)) = with(|s| {
            let item = s.queue.pop_front();
            if item.is_none() {
                s.working = false;
            }
            item.zip(s.key.clone())
        }) else {
            return;
        };
        let hash = if is_hash(&item) {
            item.to_lowercase()
        } else {
            match anabolic_core::filefacts::sha256(&item) {
                Some(h) => h.to_lowercase(),
                None => continue,
            }
        };
        let known = with(|s| {
            s.hashes.insert(item.clone(), hash.clone());
            s.reports.contains_key(&hash)
        });
        if known {
            continue;
        }
        let report = virustotal::lookup(&hash, &key);
        let wait = report == Report::Wait;
        with(|s| {
            if wait {
                s.queue.push_front(item);
            }
            s.reports.insert(hash, report);
        });
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
        std::thread::sleep(if wait {
            Duration::from_secs(60)
        } else {
            SPACING
        });
    }
}

/// The answer in a pane: its words, red when engines flag the file.
pub fn label(ui: &mut eframe::egui::Ui, path_or_hash: &str) {
    use eframe::egui::RichText;
    if let Some(r) = report(path_or_hash) {
        let color = if r.flagged() {
            crate::theme::BAD
        } else if matches!(r, Report::Known { .. }) {
            crate::theme::GOOD
        } else {
            crate::theme::TEXT_WEAK
        };
        ui.label(RichText::new(r.words()).color(color));
    }
}
