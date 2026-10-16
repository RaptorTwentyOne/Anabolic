//! Rules: standing orders for a program, Process Lasso style. A rule names
//! an executable; its settings (priority, cores, efficiency mode) are
//! applied once to each of its processes when it appears, and to the
//! running ones when the rule is saved, so a change made by hand afterwards
//! stands. Its alerts are checked on every refresh and fire once, until the
//! value falls back.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use anabolic_core::actions::{Action, Priority, Target};
use anabolic_core::{ProcKey, ProcState, Snapshot};
use eframe::egui::{self, Align, RichText, Ui};
use serde::{Deserialize, Serialize};

use crate::{fmt, theme, widgets};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// The priorities a rule may set. Realtime is left out on purpose: a
/// realtime process that spins can starve the machine, input included.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RulePriority {
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
}

impl RulePriority {
    pub const ALL: [RulePriority; 5] = [
        RulePriority::Idle,
        RulePriority::BelowNormal,
        RulePriority::Normal,
        RulePriority::AboveNormal,
        RulePriority::High,
    ];

    pub fn core(self) -> Priority {
        match self {
            RulePriority::Idle => Priority::Idle,
            RulePriority::BelowNormal => Priority::BelowNormal,
            RulePriority::Normal => Priority::Normal,
            RulePriority::AboveNormal => Priority::AboveNormal,
            RulePriority::High => Priority::High,
        }
    }

    pub fn from_core(p: Priority) -> Option<Self> {
        RulePriority::ALL.into_iter().find(|r| r.core() == p)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub enabled: bool,
    /// Executable name, matched case-insensitively ("chrome.exe").
    pub image: String,
    pub priority: Option<RulePriority>,
    /// Logical processors it may run on.
    pub affinity: Option<u64>,
    /// Windows' efficiency mode (EcoQoS).
    pub efficiency: Option<bool>,
    /// Warn when a process of it passes this much private memory, GiB.
    pub memory_alert_gb: Option<f32>,
    /// Warn when a process of it uses more than this share of the machine,
    /// percent…
    pub cpu_alert_pct: Option<f32>,
    /// …for this long, seconds.
    pub cpu_alert_secs: u32,
    /// Warn when a process of it holds more handles than this.
    pub handles_alert: Option<u32>,
    /// It is a game: game mode starts when it is in front, windowed too.
    pub game: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            enabled: true,
            image: String::new(),
            priority: None,
            affinity: None,
            efficiency: None,
            memory_alert_gb: None,
            cpu_alert_pct: None,
            cpu_alert_secs: 30,
            handles_alert: None,
            game: false,
        }
    }
}

impl Rule {
    pub fn matches(&self, name: &str) -> bool {
        self.enabled && name.eq_ignore_ascii_case(self.image.trim())
    }

    /// What it changes, in a few words.
    pub fn settings(&self) -> String {
        let mut parts = Vec::new();
        if let Some(p) = self.priority {
            parts.push(format!("{} priority", p.core().label()));
        }
        if let Some(mask) = self.affinity {
            parts.push(format!("cores {}", cores_text(mask)));
        }
        match self.efficiency {
            Some(true) => parts.push("efficiency mode".into()),
            Some(false) => parts.push("no efficiency mode".into()),
            None => {}
        }
        if self.game {
            parts.push("a game".into());
        }
        if parts.is_empty() {
            "nothing".into()
        } else {
            parts.join(" · ")
        }
    }

    pub fn alerts(&self) -> String {
        let mut parts = Vec::new();
        if let Some(gb) = self.memory_alert_gb {
            parts.push(format!("memory over {gb} GB"));
        }
        if let Some(pct) = self.cpu_alert_pct {
            parts.push(format!("CPU over {pct:.0} % for {} s", self.cpu_alert_secs));
        }
        if let Some(n) = self.handles_alert {
            parts.push(format!("over {} handles", fmt::count(n as u64)));
        }
        if parts.is_empty() {
            "none".into()
        } else {
            parts.join(" · ")
        }
    }
}

/// "0-7, 16" → mask; None when it names nothing or a core past 63.
pub fn parse_cores(text: &str) -> Option<u64> {
    let mut mask = 0u64;
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim().parse::<u32>().ok()?, b.trim().parse::<u32>().ok()?),
            None => {
                let n = part.parse::<u32>().ok()?;
                (n, n)
            }
        };
        if a > b || b > 63 {
            return None;
        }
        for n in a..=b {
            mask |= 1 << n;
        }
    }
    (mask != 0).then_some(mask)
}

/// Mask → "0-7, 16".
pub fn cores_text(mask: u64) -> String {
    let mut runs = Vec::new();
    let mut n = 0;
    while n < 64 {
        if mask >> n & 1 == 1 {
            let start = n;
            while n + 1 < 64 && mask >> (n + 1) & 1 == 1 {
                n += 1;
            }
            runs.push(if start == n {
                start.to_string()
            } else {
                format!("{start}-{n}")
            });
        }
        n += 1;
    }
    runs.join(", ")
}

/// Applies rules and watches their alerts. Lives on the UI thread; the
/// actions it returns go to the action thread.
#[derive(Default)]
pub struct Engine {
    /// (process, rule) pairs already applied.
    applied: HashSet<(ProcKey, usize)>,
    /// The rules as last seen: an edit applies them again.
    seen: Vec<Rule>,
    memory_alerted: HashSet<(ProcKey, usize)>,
    cpu_since: HashMap<(ProcKey, usize), Instant>,
    cpu_alerted: HashSet<(ProcKey, usize)>,
    handles_alerted: HashSet<(ProcKey, usize)>,
    last_seq: u64,
}

impl Engine {
    /// Once per snapshot: what to change, and what to warn about.
    pub fn step(&mut self, rules: &[Rule], snap: &Snapshot) -> (Vec<Action>, Vec<String>) {
        let mut actions = Vec::new();
        let mut alerts = Vec::new();
        if snap.seq == self.last_seq || snap.seq < 2 {
            return (actions, alerts);
        }
        self.last_seq = snap.seq;
        if self.seen != rules {
            self.applied.clear();
            self.memory_alerted.clear();
            self.cpu_since.clear();
            self.cpu_alerted.clear();
            self.handles_alerted.clear();
            self.seen = rules.to_vec();
        }
        let now = Instant::now();
        let mut live = HashSet::new();
        for p in snap
            .processes
            .iter()
            .filter(|p| p.state != ProcState::Exited && !p.is_pseudo() && p.pid != snap.own_pid)
        {
            for (i, rule) in rules.iter().enumerate() {
                if !rule.matches(&p.name) {
                    continue;
                }
                let id = (p.key, i);
                live.insert(p.key);
                if self.applied.insert(id) {
                    let target = Target {
                        key: p.key,
                        name: p.name.clone(),
                    };
                    if let Some(priority) = rule.priority
                        && Priority::from_base(p.base_priority) != Some(priority.core())
                    {
                        actions.push(Action::SetPriority(target.clone(), priority.core()));
                    }
                    if let Some(mask) = rule.affinity {
                        actions.push(Action::SetAffinity(target.clone(), mask));
                    }
                    if let Some(on) = rule.efficiency {
                        actions.push(Action::SetEfficiencyMode(target, on));
                    }
                }
                if let Some(gb) = rule.memory_alert_gb {
                    let limit = gb as f64 * GIB;
                    let used = p.private_bytes as f64;
                    if used > limit {
                        if self.memory_alerted.insert(id) {
                            alerts.push(format!(
                                "{} ({}) passed {} of private memory: it uses {} now",
                                p.name,
                                p.pid,
                                fmt::bytes(limit as u64),
                                fmt::bytes(p.private_bytes)
                            ));
                        }
                    } else if used < limit * 0.9 {
                        self.memory_alerted.remove(&id);
                    }
                }
                if let Some(limit) = rule.handles_alert {
                    if p.handles > limit {
                        if self.handles_alerted.insert(id) {
                            alerts.push(format!(
                                "{} ({}) holds {} handles, over the {} its rule allows",
                                p.name,
                                p.pid,
                                fmt::count(p.handles as u64),
                                fmt::count(limit as u64)
                            ));
                        }
                    } else if (p.handles as f64) < limit as f64 * 0.9 {
                        self.handles_alerted.remove(&id);
                    }
                }
                if let Some(pct) = rule.cpu_alert_pct {
                    if p.cpu >= pct {
                        let since = *self.cpu_since.entry(id).or_insert(now);
                        let long = Duration::from_secs(rule.cpu_alert_secs as u64);
                        if now - since >= long && self.cpu_alerted.insert(id) {
                            alerts.push(format!(
                                "{} ({}) has used over {pct:.0} % of the CPU for {} s",
                                p.name, p.pid, rule.cpu_alert_secs
                            ));
                        }
                    } else {
                        self.cpu_since.remove(&id);
                        self.cpu_alerted.remove(&id);
                    }
                }
            }
        }
        // Forget processes that are gone.
        self.applied.retain(|(k, _)| live.contains(k));
        self.memory_alerted.retain(|(k, _)| live.contains(k));
        self.cpu_since.retain(|(k, _), _| live.contains(k));
        self.cpu_alerted.retain(|(k, _)| live.contains(k));
        self.handles_alerted.retain(|(k, _)| live.contains(k));
        (actions, alerts)
    }
}

/// The programs Anabolic blocked from the network, with a way back.
pub fn blocked_list(ui: &mut Ui, blocked: &[String], out: &mut Vec<crate::proc_table::UiCmd>) {
    if blocked.is_empty() {
        return;
    }
    ui.add_space(14.0);
    widgets::section(ui, "Blocked from the network");
    ui.label(
        RichText::new(
            "Windows Firewall rules Anabolic added (named \"Anabolic: block …\"). Every process \
             started from these files is cut off, in and out.",
        )
        .size(theme::META)
        .color(theme::TEXT_WEAK),
    );
    for path in blocked {
        ui.horizontal(|ui| {
            if ui.small_button("Allow again").clicked() {
                out.push(crate::proc_table::UiCmd::Unblock(path.clone()));
            }
            ui.label(path.as_str());
        });
    }
}

/// A rule being written.
struct Draft {
    /// Index of the rule it edits; None for a new one.
    index: Option<usize>,
    rule: Rule,
    cores: String,
    error: Option<String>,
}

#[derive(Default)]
pub struct RulesPage {
    draft: Option<Draft>,
}

impl RulesPage {
    /// Opens the editor on a new rule for `image`, set to what it runs
    /// with now.
    pub fn new_rule(&mut self, image: &str, priority: Option<RulePriority>) {
        self.draft = Some(Draft {
            index: None,
            rule: Rule {
                image: image.to_string(),
                priority,
                ..Rule::default()
            },
            cores: String::new(),
            error: None,
        });
    }
}

/// Returns true when the rules changed.
pub fn show(ui: &mut Ui, page: &mut RulesPage, rules: &mut Vec<Rule>, snap: &Snapshot) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Rules").font(theme::brand(theme::TITLE)));
        ui.add_space(12.0);
        if page.draft.is_none() && ui.button("Add rule").clicked() {
            page.new_rule("", None);
        }
    });
    ui.label(
        RichText::new(
            "A rule is applied once to each process of the program when it starts, and to the running \
             ones when you save it; a change you make by hand afterwards stands. Alerts are checked on \
             every refresh.",
        )
        .color(theme::TEXT_WEAK),
    );
    ui.add_space(8.0);

    if let Some(draft) = &mut page.draft {
        match editor(ui, draft, snap) {
            Some(true) => {
                let rule = draft.rule.clone();
                match draft.index {
                    Some(i) if i < rules.len() => rules[i] = rule,
                    _ => rules.push(rule),
                }
                page.draft = None;
                changed = true;
            }
            Some(false) => page.draft = None,
            None => {}
        }
        ui.add_space(12.0);
    }

    let count = |rule: &Rule| {
        snap.processes
            .iter()
            .filter(|p| p.state != ProcState::Exited && rule.matches(&p.name))
            .count()
    };
    let columns = [
        ("On", 36.0, Align::Min),
        ("Program", 200.0, Align::Min),
        ("Settings", 300.0, Align::Min),
        ("Alerts", 260.0, Align::Min),
        ("Running", 70.0, Align::Max),
        ("", 120.0, Align::Min),
    ];
    widgets::header(ui, &columns);
    let mut edit = None;
    let mut delete = None;
    for (i, rule) in rules.iter_mut().enumerate() {
        widgets::row(ui, i, |ui| {
            widgets::cell(ui, columns[0].1, Align::Min, |ui| {
                if ui
                    .checkbox(&mut rule.enabled, "")
                    .on_hover_text("Turn the rule on or off")
                    .changed()
                {
                    changed = true;
                }
            });
            let name = RichText::new(&rule.image).color(if rule.enabled {
                theme::TEXT
            } else {
                theme::TEXT_DIM
            });
            widgets::text(ui, columns[1].1, Align::Min, name);
            widgets::text(
                ui,
                columns[2].1,
                Align::Min,
                RichText::new(rule.settings()).color(theme::TEXT_WEAK),
            );
            widgets::text(
                ui,
                columns[3].1,
                Align::Min,
                RichText::new(rule.alerts()).color(theme::TEXT_WEAK),
            );
            widgets::text(ui, columns[4].1, Align::Max, count(rule).to_string());
            widgets::cell(ui, columns[5].1, Align::Min, |ui| {
                if ui.small_button("Edit").clicked() {
                    edit = Some(i);
                }
                if ui.small_button("Delete").clicked() {
                    delete = Some(i);
                }
            });
        });
    }
    if rules.is_empty() {
        ui.label(
            RichText::new(
                "No rules yet. Add one here, or right-click a process and pick \u{201c}Create a rule\u{201d}.",
            )
            .color(theme::TEXT_DIM),
        );
    }
    if let Some(i) = edit {
        let rule = rules[i].clone();
        page.draft = Some(Draft {
            index: Some(i),
            cores: rule.affinity.map(cores_text).unwrap_or_default(),
            rule,
            error: None,
        });
    }
    if let Some(i) = delete {
        rules.remove(i);
        if page.draft.as_ref().is_some_and(|d| d.index == Some(i)) {
            page.draft = None;
        }
        changed = true;
    }
    changed
}

/// Some(true): save; Some(false): cancel.
fn editor(ui: &mut Ui, d: &mut Draft, snap: &Snapshot) -> Option<bool> {
    let mut result = None;
    egui::Frame::group(ui.style())
        .fill(theme::RAISED)
        .corner_radius(theme::RADIUS_PANEL)
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width().min(760.0));
            ui.label(widgets::strong(
                if d.index.is_some() { "Edit rule" } else { "New rule" },
                theme::BODY,
            ));
            ui.add_space(6.0);
            egui::Grid::new("rule-editor")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Program");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut d.rule.image)
                                .hint_text("chrome.exe")
                                .desired_width(220.0),
                        );
                        let mut names: Vec<&str> = snap
                            .processes
                            .iter()
                            .filter(|p| p.state != ProcState::Exited && !p.is_pseudo())
                            .map(|p| &*p.name)
                            .collect();
                        names.sort_unstable_by_key(|n| n.to_ascii_lowercase());
                        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
                        egui::ComboBox::from_id_salt("rule-pick")
                            .selected_text("running programs")
                            .width(170.0)
                            .height(320.0)
                            .show_ui(ui, |ui| {
                                for name in names {
                                    if ui.selectable_label(false, name).clicked() {
                                        d.rule.image = name.to_string();
                                    }
                                }
                            });
                    });
                    ui.end_row();

                    ui.label("Priority");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut d.rule.priority, None, "Unchanged");
                        for p in RulePriority::ALL {
                            ui.selectable_value(&mut d.rule.priority, Some(p), p.core().label());
                        }
                    });
                    ui.end_row();

                    ui.label("Cores");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut d.cores)
                                .hint_text("all")
                                .desired_width(160.0),
                        );
                        let last = snap.system.cores.len().saturating_sub(1);
                        ui.label(
                            RichText::new(format!(
                                "e.g. 0-7, 16 (this machine has 0\u{2013}{last}); empty leaves it alone"
                            ))
                            .size(theme::META)
                            .color(theme::TEXT_WEAK),
                        );
                    });
                    ui.end_row();

                    ui.label("Efficiency mode");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut d.rule.efficiency, None, "Unchanged");
                        ui.selectable_value(&mut d.rule.efficiency, Some(true), "On");
                        ui.selectable_value(&mut d.rule.efficiency, Some(false), "Off");
                    });
                    ui.end_row();

                    ui.label("Game");
                    ui.checkbox(&mut d.rule.game, "It is a game: game mode starts while it is in front")
                        .on_hover_text(
                            "Game mode notices games that fill the screen by itself; this is for \
                             games played in a window.",
                        );
                    ui.end_row();

                    ui.label("Alerts");
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            let mut on = d.rule.memory_alert_gb.is_some();
                            ui.checkbox(&mut on, "Private memory passes");
                            let mut gb = d.rule.memory_alert_gb.unwrap_or(8.0);
                            ui.add_enabled(
                                on,
                                egui::DragValue::new(&mut gb)
                                    .range(0.1..=1024.0)
                                    .speed(0.1)
                                    .suffix(" GB"),
                            );
                            d.rule.memory_alert_gb = on.then_some(gb);
                        });
                        ui.horizontal(|ui| {
                            let mut on = d.rule.cpu_alert_pct.is_some();
                            ui.checkbox(&mut on, "CPU stays above");
                            let mut pct = d.rule.cpu_alert_pct.unwrap_or(25.0);
                            ui.add_enabled(
                                on,
                                egui::DragValue::new(&mut pct).range(1.0..=100.0).suffix(" %"),
                            );
                            ui.label("of the machine for");
                            ui.add_enabled(
                                on,
                                egui::DragValue::new(&mut d.rule.cpu_alert_secs)
                                    .range(5..=3600)
                                    .suffix(" s"),
                            );
                            d.rule.cpu_alert_pct = on.then_some(pct);
                        });
                        ui.horizontal(|ui| {
                            let mut on = d.rule.handles_alert.is_some();
                            ui.checkbox(&mut on, "Handles pass")
                                .on_hover_text(
                                    "A program that keeps opening handles and never closes them \
                                     slows itself and Windows down over time.",
                                );
                            let mut n = d.rule.handles_alert.unwrap_or(50_000);
                            ui.add_enabled(
                                on,
                                egui::DragValue::new(&mut n)
                                    .range(1_000..=10_000_000)
                                    .speed(100.0),
                            );
                            d.rule.handles_alert = on.then_some(n);
                        });
                    });
                    ui.end_row();
                });
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "Priority, cores and efficiency mode are set by opening the process. Anti-cheat \
                     protected games may refuse this or treat it as suspicious.",
                )
                .size(theme::META)
                .color(theme::ELEVATED_TEXT),
            );
            if let Some(e) = &d.error {
                ui.label(RichText::new(e).color(theme::BAD));
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    d.error = None;
                    d.rule.image = d.rule.image.trim().to_string();
                    if d.rule.image.is_empty() {
                        d.error = Some("Name the program, for example chrome.exe.".into());
                    } else if d.cores.trim().is_empty() {
                        d.rule.affinity = None;
                        result = Some(true);
                    } else {
                        match parse_cores(&d.cores) {
                            Some(mask) => {
                                d.rule.affinity = Some(mask);
                                result = Some(true);
                            }
                            None => {
                                d.error = Some(
                                    "Cores: numbers and ranges such as 0-7, 16, from 0 to 63.".into(),
                                )
                            }
                        }
                    }
                }
                if ui.button("Cancel").clicked() {
                    result = Some(false);
                }
            });
        });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use anabolic_core::ProcessInfo;
    use std::sync::Arc;

    #[test]
    fn cores_round_trip() {
        assert_eq!(parse_cores("0-3, 8"), Some(0b1_0000_1111));
        assert_eq!(cores_text(0b1_0000_1111), "0-3, 8");
        assert_eq!(parse_cores(""), None);
        assert_eq!(parse_cores("3-1"), None);
        assert_eq!(parse_cores("64"), None);
    }

    fn snapshot(seq: u64, cpu: f32, private_gb: f64) -> Snapshot {
        let history = Arc::new(anabolic_core::history::History::new(4, 1, &[]));
        let mut snap = Snapshot::empty(1, history);
        snap.seq = seq;
        snap.own_pid = 1;
        let mut p = ProcessInfo::blank(
            ProcKey {
                pid: 100,
                create_time: 5,
            },
            4,
            Arc::from("Game.EXE"),
        );
        p.cpu = cpu;
        p.private_bytes = (private_gb * GIB) as u64;
        snap.processes.push(p);
        snap
    }

    #[test]
    fn applies_once_and_alerts_once() {
        let rules = vec![Rule {
            image: "game.exe".into(),
            priority: Some(RulePriority::High),
            memory_alert_gb: Some(2.0),
            ..Rule::default()
        }];
        let mut engine = Engine::default();
        let (actions, alerts) = engine.step(&rules, &snapshot(2, 0.0, 3.0));
        assert_eq!(actions.len(), 1);
        assert_eq!(alerts.len(), 1);
        // The next refresh changes nothing and says nothing new.
        let (actions, alerts) = engine.step(&rules, &snapshot(3, 0.0, 3.0));
        assert!(actions.is_empty() && alerts.is_empty());
        // Back under 90 % of the limit, then over again: a new alert.
        engine.step(&rules, &snapshot(4, 0.0, 1.0));
        let (_, alerts) = engine.step(&rules, &snapshot(5, 0.0, 3.0));
        assert_eq!(alerts.len(), 1);
    }
}
