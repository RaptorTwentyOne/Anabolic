// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Anabolic without a window, for `anabolic mcp` and the terminal commands:
//! the collector and what the MCP server's tools share, kept going by a
//! thread that does for them what the window's logic does otherwise. It
//! turns on the kernel traces a tool is waiting for (and off again), and
//! watches for leaks. Nothing is recorded and no setting is saved: the
//! window, when it runs, owns those.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anabolic_core::{Collector, CollectorConfig, Command};
use arc_swap::ArcSwap;
use crossbeam_channel::Receiver;
use eframe::egui;

use crate::mcp::{McpEvent, Shared, Trace, Wants};
use crate::settings::McpSettings;

pub struct Headless {
    pub shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Headless {
    /// Starts reading the system every `interval`. `allowed` is what MCP
    /// tools may do. With `log`, what the server reports (a client
    /// connecting, a failure) goes to standard error.
    pub fn start(mcp: McpSettings, interval: Duration, log: bool) -> Headless {
        let collector = Collector::start(
            CollectorConfig {
                interval,
                ..Default::default()
            },
            || {},
        );
        let (events_tx, events) = crossbeam_channel::unbounded();
        let shared = Arc::new(Shared {
            snapshot: collector.shared(),
            collector: collector.commands(),
            settings: Arc::new(ArcSwap::from_pointee(mcp)),
            leaks: Arc::new(ArcSwap::from_pointee(Vec::new())),
            wants: Arc::new(Wants::default()),
            events: events_tx,
            // Nothing draws; the tools only ask it to repaint.
            ctx: egui::Context::default(),
            data_dir: crate::data_dir(),
            elevated: anabolic_core::hardening::is_elevated(),
            recordings: Mutex::new(Default::default()),
            headless: true,
        });
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let shared = shared.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("anabolic-headless".into())
                .spawn(move || keep(collector, &shared, &events, &stop, log))
                .ok()
        };
        Headless {
            shared,
            stop,
            thread,
        }
    }
}

impl Drop for Headless {
    /// Stops the traces it turned on and the collector, and lifts what
    /// limits a tool set (none outlive Anabolic).
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        anabolic_core::limits::lift_all();
    }
}

/// The traces a tool can wait for, in the order `keep` tracks them.
const TRACES: [Trace; 4] = [Trace::Network, Trace::Cores, Trace::Disks, Trace::Frames];

fn keep(
    collector: Collector,
    shared: &Shared,
    events: &Receiver<McpEvent>,
    stop: &AtomicBool,
    log: bool,
) {
    let mut on = [false; TRACES.len()];
    let mut growth = anabolic_core::growth::Growth::default();
    let mut seen = 0;
    while !stop.load(Ordering::Relaxed) {
        // Details trickling in for the window's table: nobody shows them.
        for _ in collector.details().try_iter() {}
        for event in events.try_iter() {
            if log {
                report(&event);
            }
        }
        for (i, trace) in TRACES.into_iter().enumerate() {
            let want = shared.wants.wanted(trace);
            if want != on[i] {
                on[i] = want;
                switch(&collector, trace, want);
            }
        }
        let snap = shared.snapshot.load();
        if snap.seq != seen {
            seen = snap.seq;
            growth.observe(&snap);
            shared.leaks.store(Arc::new(growth.leaks().to_vec()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    for (i, trace) in TRACES.into_iter().enumerate() {
        if on[i] {
            switch(&collector, trace, false);
        }
    }
    collector.shutdown();
}

fn switch(collector: &Collector, trace: Trace, on: bool) {
    match trace {
        Trace::Network => collector.send(Command::NetActivity(on)),
        Trace::Cores => collector.send(Command::TraceCores(on)),
        Trace::Disks => collector.send(Command::TraceDisks(on)),
        Trace::Frames => {
            collector.send(Command::TraceFrames(on));
            collector.send(Command::TraceLatency(on));
        }
    }
}

/// What the server reports, for whoever reads standard error.
fn report(event: &McpEvent) {
    match event {
        McpEvent::Listening(addr) => eprintln!("anabolic: serving MCP at http://{addr}/mcp"),
        McpEvent::Failed(e) => eprintln!("anabolic: {e}"),
        McpEvent::Client { name, version } => {
            eprintln!("anabolic: {name} {version} connected")
        }
        McpEvent::Call { .. } | McpEvent::Blocked { .. } => {}
    }
}

/// The MCP settings the window saved (its MCP page): what clients may do,
/// the port and the access key. The defaults when there are none, or when
/// they cannot be read.
pub fn saved_mcp_settings() -> McpSettings {
    saved_settings().map(|s| s.mcp).unwrap_or_default()
}

/// The window's settings, as eframe keeps them: a RON map whose "app"
/// entry is the settings, written as RON in turn.
fn saved_settings() -> Option<crate::settings::Settings> {
    let file = settings_file()?;
    let text = std::fs::read_to_string(file).ok()?;
    let entries: std::collections::HashMap<String, String> = ron::from_str(&text).ok()?;
    ron::from_str(entries.get(eframe::APP_KEY)?).ok()
}

/// Where the window keeps its settings (see `run`).
fn settings_file() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("ANABOLIC_DATA_DIR") {
        return Some(std::path::Path::new(&dir).join("app.ron"));
    }
    #[cfg(windows)]
    {
        eframe::storage_dir("Anabolic").map(|dir| dir.join("app.ron"))
    }
    #[cfg(target_os = "linux")]
    {
        crate::data_dir().map(|dir| dir.join("app.ron"))
    }
}
