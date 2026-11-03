// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! CPU and memory limits on the window's side (the holding is
//! `anabolic_core::limits`): which processes a limit holds and to what,
//! for the process list and the details, and the dialog that sets one.

use std::collections::HashMap;
use std::sync::Arc;

use anabolic_core::ProcKey;
use anabolic_core::actions::Target;
use anabolic_core::limits::{Applied, Limits};
use eframe::egui::{self, Id, Modal, RichText};

use crate::{fmt, theme, widgets};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// What holds a process.
#[derive(Clone, Debug, PartialEq)]
pub struct Holder {
    /// "rule:chrome.exe", "rule:chrome.exe/1234:…" (a condition's, on one
    /// process), "process:…".
    pub group: Arc<str>,
    pub limits: Limits,
}

impl Holder {
    /// The rule's program, when a rule holds it.
    pub fn rule(&self) -> Option<&str> {
        let rest = self.group.strip_prefix("rule:")?;
        Some(rest.split_once('/').map_or(rest, |(image, _)| image))
    }
}

/// What a limit, or lifting one, came to, to tell: the text and whether it
/// is a failure. A rule's limits work quietly; one asked for by hand says
/// so.
pub fn told(a: &Applied, names: impl Fn(ProcKey) -> Option<Arc<str>>) -> Vec<(String, bool)> {
    let mut tell = Vec::new();
    let rule = a
        .group
        .strip_prefix("rule:")
        .map(|r| r.split_once('/').map_or(r, |(image, _)| image));
    let name = a.joined.iter().find_map(|&k| names(k));
    if a.limits.is_empty() {
        if let Some(e) = &a.error {
            tell.push((format!("Could not lift the limit: {e}"), true));
        } else if rule.is_none() && a.before.is_some() {
            tell.push(("The limit is lifted".to_string(), false));
        }
        return tell;
    }
    if let Some(e) = &a.error {
        let whose = match rule {
            Some(image) => format!("the limit of the rule for {image}"),
            None => "the limit".into(),
        };
        tell.push((format!("Could not set {whose}: {e}"), true));
        return tell;
    }
    if rule.is_none() && !a.joined.is_empty() {
        let what = text(&a.limits);
        tell.push((
            match a.before {
                Some(_) => format!(
                    "{} is limited to {what} now",
                    name.as_deref().unwrap_or("The process")
                ),
                None => format!(
                    "Limited {} to {what}",
                    name.as_deref().unwrap_or("the process")
                ),
            },
            false,
        ));
    }
    for (t, why) in &a.failed {
        tell.push((
            format!("Could not limit {} ({}): {why}", t.name, t.key.pid),
            true,
        ));
    }
    tell
}

/// What holds each process, from what is held: its own limit before its
/// rule's (the one it was given by hand).
fn by_process(held: &[anabolic_core::limits::Held]) -> HashMap<ProcKey, Holder> {
    let mut by_process = HashMap::new();
    for h in held {
        let group: Arc<str> = h.group.as_str().into();
        for &k in &h.members {
            let holder = Holder {
                group: group.clone(),
                limits: h.limits,
            };
            by_process
                .entry(k)
                .and_modify(|found: &mut Holder| {
                    if holder.rule().is_none() {
                        *found = holder.clone();
                    }
                })
                .or_insert(holder);
        }
    }
    by_process
}

/// What holds `key` now, asked of the holder of limits itself.
pub fn holder_now(key: ProcKey) -> Option<Holder> {
    by_process(&anabolic_core::limits::held()).remove(&key)
}

/// Hands what holds whom to the process list and the details, when it
/// changed since `seen`.
pub fn publish(ctx: &egui::Context, seen: &mut Option<u64>) {
    let now = anabolic_core::limits::generation();
    if *seen == Some(now) {
        return;
    }
    *seen = Some(now);
    let held = by_process(&anabolic_core::limits::held());
    ctx.data_mut(|d| d.insert_temp(Id::new(HELD), Arc::new(held)));
}

const HELD: &str = "held limits";

/// What holds `key`, as last published.
pub fn holder(ctx: &egui::Context, key: ProcKey) -> Option<Holder> {
    ctx.data(|d| d.get_temp::<Arc<HashMap<ProcKey, Holder>>>(Id::new(HELD)))
        .and_then(|held| held.get(&key).cloned())
}

/// "25 % CPU · 4.00 GB memory".
pub fn text(l: &Limits) -> String {
    let mut parts = Vec::new();
    if let Some(pct) = l.cpu_pct {
        parts.push(format!("{} % CPU", trim(pct)));
    }
    if let Some(bytes) = l.memory {
        parts.push(format!("{} memory", fmt::bytes(bytes)));
    }
    parts.join(" · ")
}

/// 25 → "25", 2.5 → "2.5".
fn trim(v: f32) -> String {
    let s = format!("{v:.1}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

/// A process can be limited: why not, when not.
pub fn refused(name: &str, own: bool) -> Option<String> {
    if own {
        return Some("Anabolic does not limit itself.".into());
    }
    if crate::gamemode::untouchable(name) {
        return Some("Windows' core processes and anti-cheat programs are left alone.".into());
    }
    anabolic_core::limits::available().err()
}

pub struct LimitDialog {
    pub target: Target,
    /// What its own limit is now, if it has one.
    pub current: Option<Limits>,
    /// A rule's limit that holds it as well.
    pub rule: Option<(String, Limits)>,
    pub cpu_on: bool,
    pub cpu_pct: f32,
    pub memory_on: bool,
    pub memory_gb: f32,
    /// Its private memory now, bytes.
    pub using: u64,
    pub logical: usize,
}

pub enum LimitChoice {
    Pending,
    Apply(Limits),
    Lift,
    Cancel,
}

impl LimitDialog {
    pub fn new(target: Target, holder: Option<Holder>, using: u64, logical: usize) -> Self {
        let (current, rule) = match holder {
            Some(h) => match h.rule() {
                Some(image) => (None, Some((image.to_string(), h.limits))),
                None => (Some(h.limits), None),
            },
            None => (None, None),
        };
        let start = current.unwrap_or_default();
        // A starting point above what it uses: a limit under that fails it
        // at once.
        let suggested = ((using as f64 * 1.5 / GIB).max(0.5) * 2.0).ceil() / 2.0;
        Self {
            target,
            current,
            rule,
            cpu_on: start.cpu_pct.is_some() || start.is_empty(),
            cpu_pct: start.cpu_pct.unwrap_or(25.0),
            memory_on: start.memory.is_some(),
            memory_gb: start
                .memory
                .map_or(suggested as f32, |b| (b as f64 / GIB) as f32),
            using,
            logical,
        }
    }

    fn limits(&self) -> Limits {
        Limits {
            cpu_pct: self.cpu_on.then_some(self.cpu_pct),
            memory: self
                .memory_on
                .then_some((self.memory_gb as f64 * GIB) as u64),
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> LimitChoice {
        let mut choice = LimitChoice::Pending;
        let response = Modal::new(Id::new("limit-dialog")).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.label(widgets::strong("Limit CPU and memory", theme::SUBTITLE));
            ui.label(
                RichText::new(format!(
                    "{} (PID {}), and the processes it starts from now on",
                    self.target.name, self.target.key.pid
                ))
                .color(theme::TEXT_WEAK),
            );
            ui.add_space(10.0);
            egui::Grid::new("limit-grid")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.checkbox(&mut self.cpu_on, "CPU");
                    ui.horizontal(|ui| {
                        ui.label("at most");
                        ui.add_enabled(
                            self.cpu_on,
                            egui::DragValue::new(&mut self.cpu_pct)
                                .range(1.0..=99.0)
                                .speed(0.5)
                                .max_decimals(1)
                                .suffix(" %"),
                        );
                        let cores = self.cpu_pct as f64 / 100.0 * self.logical as f64;
                        ui.label(
                            RichText::new(format!(
                                "of the whole machine: about {} of its {} logical processors",
                                trim(cores as f32),
                                self.logical
                            ))
                            .color(theme::TEXT_WEAK),
                        );
                    });
                    ui.end_row();
                    ui.checkbox(&mut self.memory_on, "Memory");
                    ui.horizontal(|ui| {
                        ui.label("at most");
                        ui.add_enabled(
                            self.memory_on,
                            egui::DragValue::new(&mut self.memory_gb)
                                .range(0.1..=1024.0)
                                .speed(0.05)
                                .max_decimals(2)
                                .suffix(" GB"),
                        );
                        ui.label(
                            RichText::new(format!("of private memory; it uses {} now", fmt::bytes(self.using)))
                                .color(theme::TEXT_WEAK),
                        );
                    });
                    ui.end_row();
                });
            ui.add_space(6.0);
            let note = |ui: &mut egui::Ui, text: &str| {
                ui.label(RichText::new(text).size(theme::META).color(theme::TEXT_WEAK));
            };
            note(
                ui,
                "Windows holds it to this share however busy the machine is, whatever its \
                 priority. Past the memory limit its requests for more fail: most programs then \
                 stop with an error.",
            );
            note(ui, "The limit lasts until it exits, or until Anabolic quits.");
            if let Some((image, limits)) = &self.rule {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "The rule for {image} holds it to {} as well; the tighter of the two counts.",
                        text(limits)
                    ))
                    .size(theme::META)
                    .color(theme::ACCENT),
                );
            }
            if self.memory_on && (self.memory_gb as f64 * GIB) < self.using as f64 {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("That is less than it uses now: it would fail at once.")
                        .size(theme::META)
                        .color(theme::ELEVATED_TEXT),
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let limits = self.limits();
                let label = if self.current.is_some() { "Change" } else { "Limit" };
                if ui.add_enabled(!limits.is_empty(), egui::Button::new(label)).clicked() {
                    choice = LimitChoice::Apply(limits);
                }
                if self.current.is_some() && ui.button("Remove the limit").clicked() {
                    choice = LimitChoice::Lift;
                }
                if ui.button("Cancel").clicked() {
                    choice = LimitChoice::Cancel;
                }
            });
        });
        if matches!(choice, LimitChoice::Pending) && response.should_close() {
            choice = LimitChoice::Cancel;
        }
        choice
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(pid: u32) -> ProcKey {
        ProcKey {
            pid,
            create_time: pid as i64,
        }
    }

    fn applied(group: &str, limits: Limits, joined: &[u32]) -> Applied {
        Applied {
            group: group.into(),
            limits,
            before: None,
            joined: joined.iter().map(|&p| key(p)).collect(),
            failed: Vec::new(),
            error: None,
        }
    }

    #[test]
    fn says_so_for_a_limit_by_hand_and_keeps_quiet_for_a_rules() {
        let names = |_| Some(Arc::from("app.exe"));
        let cpu = Limits {
            cpu_pct: Some(25.0),
            memory: None,
        };
        let told_for = |group: &str, before| {
            told(
                &Applied {
                    before,
                    ..applied(group, cpu, &[7])
                },
                names,
            )
        };
        assert_eq!(
            told_for("process:7:7", None),
            [("Limited app.exe to 25 % CPU".to_string(), false)]
        );
        assert_eq!(
            told_for("process:7:7", Some(cpu)),
            [("app.exe is limited to 25 % CPU now".to_string(), false)]
        );
        assert!(told_for("rule:app.exe", None).is_empty());
        // A rule's that fails says so.
        let failed = Applied {
            failed: vec![(
                Target {
                    key: key(8),
                    name: "app.exe".into(),
                },
                "denied".into(),
            )],
            ..applied("rule:app.exe", cpu, &[])
        };
        assert_eq!(
            told(&failed, names),
            [("Could not limit app.exe (8): denied".to_string(), true)]
        );
    }

    #[test]
    fn a_processs_own_limit_is_the_one_shown() {
        let held = [
            anabolic_core::limits::Held {
                group: "rule:app.exe".into(),
                limits: Limits::default(),
                members: vec![key(7), key(8)],
            },
            anabolic_core::limits::Held {
                group: "process:7:7".into(),
                limits: Limits::default(),
                members: vec![key(7)],
            },
        ];
        let by = by_process(&held);
        assert_eq!(&*by[&key(7)].group, "process:7:7");
        assert_eq!(by[&key(8)].rule(), Some("app.exe"));
        let condition = Holder {
            group: "rule:app.exe/8:8".into(),
            limits: Limits::default(),
        };
        assert_eq!(condition.rule(), Some("app.exe"));
    }

    #[test]
    fn says_what_a_limit_is() {
        let l = Limits {
            cpu_pct: Some(12.5),
            memory: Some(2 * 1024 * 1024 * 1024),
        };
        assert_eq!(
            text(&l),
            format!("12.5 % CPU · {} memory", fmt::bytes(2 << 30))
        );
    }
}
