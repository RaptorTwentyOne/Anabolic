// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The standby list cleared by itself, in the manner of ISLC (Intelligent
//! standby list cleaner): when the list holds at least a set amount and
//! free memory falls under another, and, if asked, only while a game is
//! in front. Windows hands standby pages to whoever needs memory anyway;
//! clearing them ahead helps the games that stutter while it does that
//! under pressure, and costs the cache: files read before are read from
//! disk again. At most once a minute, on a thread of its own.

use std::time::{Duration, Instant, SystemTime};

use anabolic_core::Snapshot;
use anabolic_core::memtools::Purge;
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, RichText, Ui};
use serde::{Deserialize, Serialize};

use crate::{fmt, theme, widgets};

const GB: f64 = (1u64 << 30) as f64;
/// Not more often than this, whatever the lists say.
const AT_MOST: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StandbySettings {
    pub enabled: bool,
    /// When the standby list holds at least this, GB…
    pub standby_gb: f32,
    /// …and free memory is under this, GB.
    pub free_gb: f32,
    /// Only while a game is in front.
    pub only_playing: bool,
    /// Only the lowest-priority standby pages: gentler, frees less.
    pub low_priority_only: bool,
}

impl Default for StandbySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            standby_gb: 1.0,
            free_gb: 2.0,
            only_playing: true,
            low_priority_only: false,
        }
    }
}

#[derive(Default)]
pub struct Cleaner {
    last_seq: u64,
    last: Option<Instant>,
    running: Option<Receiver<Result<(), String>>>,
    /// This session: how many times, when last, and how much the list held.
    pub times: u32,
    pub last_at: Option<SystemTime>,
    pub cleared: u64,
    /// Why it could not, once it could not: it stops trying.
    pub failed: Option<String>,
}

impl Cleaner {
    /// Clears the list when the settings say so; call with each snapshot.
    pub fn step(&mut self, s: &StandbySettings, snap: &Snapshot, playing: bool) {
        if let Some(rx) = &self.running
            && let Ok(result) = rx.try_recv()
        {
            self.running = None;
            if let Err(e) = result {
                self.failed = Some(e);
            }
        }
        if !s.enabled || snap.seq == self.last_seq || self.failed.is_some() {
            return;
        }
        self.last_seq = snap.seq;
        let Some(lists) = snap.system.memory.lists else {
            return;
        };
        let free = lists.free + lists.zeroed;
        if !due(s, lists.standby, free, playing)
            || self.running.is_some()
            || self.last.is_some_and(|t| t.elapsed() < AT_MOST)
        {
            return;
        }
        let what = if s.low_priority_only {
            Purge::StandbyLowPriority
        } else {
            Purge::Standby
        };
        let (tx, rx) = bounded(1);
        let spawned = std::thread::Builder::new()
            .name("standby-cleaner".into())
            .spawn(move || {
                let _ = tx.send(anabolic_core::memtools::purge(what));
            });
        if spawned.is_ok() {
            self.running = Some(rx);
            self.last = Some(Instant::now());
            self.last_at = Some(SystemTime::now());
            self.times += 1;
            self.cleared += lists.standby;
        }
    }
}

/// The settings' condition: enough on standby, too little free.
fn due(s: &StandbySettings, standby: u64, free: u64, playing: bool) -> bool {
    (playing || !s.only_playing)
        && standby as f64 >= s.standby_gb as f64 * GB
        && (free as f64) < s.free_gb as f64 * GB
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn card(ui: &mut Ui, s: &mut StandbySettings, c: &Cleaner, snap: &Snapshot) {
    egui::Frame::new()
        .fill(theme::RAISED)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(theme::RADIUS_PANEL)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(widgets::strong("Standby list cleaner", theme::SUBTITLE));
                ui.add_space(8.0);
                ui.checkbox(&mut s.enabled, "On");
                ui.add_space(12.0);
                let status = if let Some(e) = &c.failed {
                    RichText::new(format!("Stopped: {e}")).color(theme::BAD)
                } else if let Some(at) = c.last_at {
                    RichText::new(format!(
                        "Cleared {} time{} this session, last at {}",
                        c.times,
                        if c.times == 1 { "" } else { "s" },
                        fmt::clock(at, false)
                    ))
                    .color(theme::GOOD)
                } else if let Some(l) = snap.system.memory.lists {
                    RichText::new(format!(
                        "Standby {}  ·  free {}",
                        fmt::bytes(l.standby),
                        fmt::bytes(l.free + l.zeroed)
                    ))
                    .color(theme::TEXT_WEAK)
                } else {
                    RichText::new("Windows shows the memory lists only to an administrator")
                        .color(theme::ELEVATED_TEXT)
                };
                ui.label(status);
            });
            ui.label(
                RichText::new(
                    "In the manner of ISLC. Windows keeps files it read in memory, on standby, \
                     and hands those pages to whoever needs memory. Under pressure that handing \
                     over can make a game stutter; clearing the list ahead avoids it, at the cost \
                     of reading those files from disk again. At most once a minute.",
                )
                .size(theme::META)
                .color(theme::TEXT_WEAK),
            );
            ui.add_space(4.0);
            ui.add_enabled_ui(s.enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Clear it when it holds at least");
                    ui.add(
                        egui::DragValue::new(&mut s.standby_gb)
                            .range(0.25..=256.0)
                            .speed(0.1)
                            .max_decimals(2)
                            .suffix(" GB"),
                    );
                    ui.label("and free memory is under");
                    ui.add(
                        egui::DragValue::new(&mut s.free_gb)
                            .range(0.25..=256.0)
                            .speed(0.1)
                            .max_decimals(2)
                            .suffix(" GB"),
                    );
                });
                ui.checkbox(&mut s.only_playing, "Only while a game is in front")
                    .on_hover_text(
                        "A game is a full-screen program in front, or one a rule marks as a game.",
                    );
                ui.checkbox(
                    &mut s.low_priority_only,
                    "Only its lowest-priority pages (gentler; frees less)",
                );
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clears_only_when_both_hold() {
        let s = StandbySettings {
            enabled: true,
            ..StandbySettings::default()
        };
        let gb = |g: f64| (g * GB) as u64;
        assert!(due(&s, gb(3.0), gb(1.0), true));
        // Not enough on standby, or enough free.
        assert!(!due(&s, gb(0.5), gb(1.0), true));
        assert!(!due(&s, gb(3.0), gb(4.0), true));
        // Not playing, when asked to wait for a game.
        assert!(!due(&s, gb(3.0), gb(1.0), false));
        let any = StandbySettings {
            only_playing: false,
            ..s
        };
        assert!(due(&any, gb(3.0), gb(1.0), false));
    }
}
