// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Rules: standing orders for a program, Process Lasso style. A rule names
//! an executable; its settings (priority, cores, efficiency mode, CPU and
//! memory limits) are applied once to each of its processes when it
//! appears, and to the running ones when the rule is saved, so a change made
//! by hand afterwards stands. Priority, efficiency mode and the CPU limit
//! may instead wait for a condition (busy, or in the background) and are
//! put back when it passes. Its watches (memory, CPU, handles, a window
//! that stops responding) are checked on every refresh and fire once, until
//! the value falls back: they tell, and may write a dump or end the process.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use anabolic_core::actions::{Action, Priority, Target};
use anabolic_core::limits::Limits;
use anabolic_core::{ProcKey, ProcState, ProcessInfo, Snapshot};
use eframe::egui::{self, Align, RichText, Ui};
use serde::{Deserialize, Serialize};

use crate::{fmt, theme, widgets};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
/// A busy process must stay under half its threshold this long to be put
/// back.
const CALM_FOR: Duration = Duration::from_secs(10);
/// A program must stay out of the front this long to count as in the
/// background: a look at another window is not leaving it.
const AWAY_FOR: Duration = Duration::from_secs(2);
/// A rule writes at most one dump this often: a program that keeps hanging
/// does not fill the disk.
const DUMP_EVERY: Duration = Duration::from_secs(600);

/// What a rule's watch does when it fires, besides telling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    #[default]
    Tell,
    Dump(Dump),
    End,
    /// The dump first: it sees the process as it was.
    DumpThenEnd(Dump),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dump {
    Small,
    Full,
}

impl Dump {
    pub fn kind(self) -> anabolic_core::dump::DumpKind {
        match self {
            Dump::Small => anabolic_core::dump::DumpKind::Small,
            Dump::Full => anabolic_core::dump::DumpKind::Full,
        }
    }
}

impl Response {
    pub const ALL: [Response; 6] = [
        Response::Tell,
        Response::Dump(Dump::Small),
        Response::Dump(Dump::Full),
        Response::End,
        Response::DumpThenEnd(Dump::Small),
        Response::DumpThenEnd(Dump::Full),
    ];

    pub fn label(self) -> &'static str {
        match self {
            Response::Tell => "tell me",
            Response::Dump(Dump::Small) => "write a small dump",
            Response::Dump(Dump::Full) => "write a full dump",
            Response::End => "end it",
            Response::DumpThenEnd(Dump::Small) => "write a small dump, then end it",
            Response::DumpThenEnd(Dump::Full) => "write a full dump, then end it",
        }
    }

    /// What it is doing, for the notification.
    fn doing(self) -> &'static str {
        match self {
            Response::Tell => "",
            Response::Dump(Dump::Small) => "writing a small dump",
            Response::Dump(Dump::Full) => "writing a full dump",
            Response::End => "ending it",
            Response::DumpThenEnd(Dump::Small) => "writing a small dump, then ending it",
            Response::DumpThenEnd(Dump::Full) => "writing a full dump, then ending it",
        }
    }

    fn dumps(self) -> bool {
        matches!(self, Response::Dump(_) | Response::DumpThenEnd(_))
    }

    /// The same without its dump.
    fn without_dump(self) -> Self {
        match self {
            Response::Dump(_) => Response::Tell,
            Response::DumpThenEnd(_) => Response::End,
            other => other,
        }
    }
}

/// A watch that fired: what to tell, and what to do.
#[derive(Debug)]
pub struct Fired {
    pub text: String,
    pub target: Target,
    pub response: Response,
}

/// When a rule's priority, efficiency mode and CPU limit apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum When {
    /// From the moment a process of it appears.
    #[default]
    Always,
    /// While a process of it uses over `pct` % of the machine, from `secs`
    /// seconds on; put back once it has stayed under half of that for ten.
    Busy { pct: f32, secs: u32 },
    /// While none of its windows is in front.
    Background,
}

impl When {
    fn text(self) -> Option<String> {
        match self {
            When::Always => None,
            When::Busy { pct, secs } => Some(format!("while over {pct:.0} % CPU for {secs} s")),
            When::Background => Some("while in the background".into()),
        }
    }
}

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
    /// Warn when a window of it has not responded this long, seconds.
    pub hang_secs: Option<u32>,
    /// What each watch does when it fires, besides telling.
    pub on_memory: Response,
    pub on_cpu: Response,
    pub on_handles: Response,
    pub on_hang: Response,
    /// It is a game: game mode starts when it is in front, windowed too.
    pub game: bool,
    /// Hold its processes, with what they start, to this share of the
    /// machine, percent: a cap Windows keeps, not a priority it weighs.
    pub cpu_limit_pct: Option<f32>,
    /// Hold all of them together to this much private memory, GiB.
    pub memory_limit_gb: Option<f32>,
    /// When its priority, efficiency mode and CPU limit apply.
    pub when: When,
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
            hang_secs: None,
            on_memory: Response::Tell,
            on_cpu: Response::Tell,
            on_handles: Response::Tell,
            on_hang: Response::Tell,
            game: false,
            cpu_limit_pct: None,
            memory_limit_gb: None,
            when: When::Always,
        }
    }
}

impl Rule {
    pub fn matches(&self, name: &str) -> bool {
        self.enabled && name.eq_ignore_ascii_case(self.image.trim())
    }

    /// What it changes, in a few words.
    pub fn settings(&self) -> String {
        // What waits for its condition…
        let mut held = Vec::new();
        if let Some(p) = self.priority {
            held.push(format!("{} priority", p.core().label()));
        }
        match self.efficiency {
            Some(true) => held.push("efficiency mode".into()),
            Some(false) => held.push("no efficiency mode".into()),
            None => {}
        }
        if let Some(pct) = self.cpu_limit_pct {
            held.push(format!("at most {pct:.0} % CPU"));
        }
        // …and what does not.
        let mut always = Vec::new();
        if let Some(mask) = self.affinity {
            always.push(format!("cores {}", cores_text(mask)));
        }
        if let Some(gb) = self.memory_limit_gb {
            always.push(format!("at most {gb} GB memory"));
        }
        if self.game {
            always.push("a game".into());
        }
        let mut parts = Vec::new();
        if !held.is_empty() {
            let held = held.join(" · ");
            parts.push(match self.when.text() {
                Some(when) => format!("{held} {when}"),
                None => held,
            });
        }
        parts.extend(always);
        if parts.is_empty() {
            "nothing".into()
        } else {
            parts.join(" · ")
        }
    }

    /// The limits on all its processes together, from the start.
    fn group_limits(&self) -> Limits {
        Limits {
            cpu_pct: self.cpu_limit_pct.filter(|_| self.when == When::Always),
            memory: self.memory_limit_gb.map(|gb| (gb as f64 * GIB) as u64),
        }
    }

    pub fn alerts(&self) -> String {
        let then = |r: Response| match r {
            Response::Tell => String::new(),
            r => format!(": {}", r.label()),
        };
        let mut parts = Vec::new();
        if let Some(gb) = self.memory_alert_gb {
            parts.push(format!("memory over {gb} GB{}", then(self.on_memory)));
        }
        if let Some(pct) = self.cpu_alert_pct {
            parts.push(format!(
                "CPU over {pct:.0} % for {} s{}",
                self.cpu_alert_secs,
                then(self.on_cpu)
            ));
        }
        if let Some(n) = self.handles_alert {
            parts.push(format!(
                "over {} handles{}",
                fmt::count(n as u64),
                then(self.on_handles)
            ));
        }
        if let Some(secs) = self.hang_secs {
            parts.push(format!("not responding for {secs} s{}", then(self.on_hang)));
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
    hang_since: HashMap<(ProcKey, usize), Instant>,
    hang_alerted: HashSet<(ProcKey, usize)>,
    /// When each rule's program last had a dump written, by name.
    last_dump: HashMap<String, Instant>,
    cpu_since: HashMap<(ProcKey, usize), Instant>,
    cpu_alerted: HashSet<(ProcKey, usize)>,
    handles_alerted: HashSet<(ProcKey, usize)>,
    /// What rules' conditions changed, per process and rule.
    conditioned: HashMap<(ProcKey, usize), Conditioned>,
    /// The programs whose limits the rules set.
    limited: HashSet<String>,
    last_seq: u64,
}

/// A rule's condition on one process: since when it holds, and what it
/// changed, to put back.
#[derive(Default)]
struct Conditioned {
    /// It has held since (not applied yet).
    since: Option<Instant>,
    applied: bool,
    /// The priority before, when the rule changed it.
    original: Option<Priority>,
    /// The priority the rule set.
    set: Option<Priority>,
    /// Efficiency mode was turned on.
    efficient: bool,
    /// The group of its CPU limit.
    limit: Option<String>,
    /// It has been calm, or back in front, since.
    calm_since: Option<Instant>,
}

impl Engine {
    /// Once per snapshot: what to change, and what to warn about.
    pub fn step(&mut self, rules: &[Rule], snap: &Snapshot) -> (Vec<Action>, Vec<Fired>) {
        if snap.seq == self.last_seq || snap.seq < 2 {
            return (Vec::new(), Vec::new());
        }
        // The program in front, by name; None when that cannot be told.
        let front = rules
            .iter()
            .any(|r| r.enabled && r.when == When::Background)
            .then(anabolic_core::foreground::front)
            .flatten()
            .and_then(|f| snap.processes.iter().find(|p| p.pid == f.pid))
            .map(|p| p.name.to_string());
        self.step_at(rules, snap, front.as_deref(), Instant::now())
    }

    fn step_at(
        &mut self,
        rules: &[Rule],
        snap: &Snapshot,
        front: Option<&str>,
        now: Instant,
    ) -> (Vec<Action>, Vec<Fired>) {
        let mut actions = Vec::new();
        let mut alerts = Vec::new();
        self.last_seq = snap.seq;
        if self.seen != rules {
            // What conditions changed goes back; the rules apply afresh.
            for ((key, _), c) in self.conditioned.drain() {
                if let Some(p) = snap.find(key) {
                    restore(p, c, &mut actions);
                }
            }
            let wanted: HashSet<String> = rules
                .iter()
                .filter(|r| r.enabled && !r.group_limits().is_empty())
                .map(|r| anabolic_core::limits::rule_group(&r.image))
                .collect();
            for group in self.limited.difference(&wanted) {
                actions.push(Action::Lift(group.clone()));
            }
            self.limited = wanted.intersection(&self.limited).cloned().collect();
            self.applied.clear();
            self.memory_alerted.clear();
            self.cpu_since.clear();
            self.cpu_alerted.clear();
            self.handles_alerted.clear();
            self.hang_since.clear();
            self.hang_alerted.clear();
            self.seen = rules.to_vec();
        }
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
                    if rule.when == When::Always {
                        if let Some(priority) = rule.priority
                            && Priority::from_base(p.base_priority) != Some(priority.core())
                        {
                            actions.push(Action::SetPriority(target(p), priority.core()));
                        }
                        if let Some(on) = rule.efficiency {
                            actions.push(Action::SetEfficiencyMode(target(p), on));
                        }
                    }
                    if let Some(mask) = rule.affinity {
                        actions.push(Action::SetAffinity(target(p), mask));
                    }
                    let limits = rule.group_limits();
                    if !limits.is_empty() {
                        let group = anabolic_core::limits::rule_group(&rule.image);
                        actions.push(Action::Limit {
                            group: group.clone(),
                            targets: vec![target(p)],
                            limits,
                        });
                        self.limited.insert(group);
                    }
                }
                self.condition(id, p, rule, front, now, &mut actions);
                if let Some(gb) = rule.memory_alert_gb {
                    let limit = gb as f64 * GIB;
                    let used = p.private_bytes as f64;
                    if used > limit {
                        if self.memory_alerted.insert(id) {
                            let what = format!(
                                "{} ({}) passed {} of private memory: it uses {} now",
                                p.name,
                                p.pid,
                                fmt::bytes(limit as u64),
                                fmt::bytes(p.private_bytes)
                            );
                            alerts.push(self.fire(p, rule, rule.on_memory, what, now));
                        }
                    } else if used < limit * 0.9 {
                        self.memory_alerted.remove(&id);
                    }
                }
                if let Some(limit) = rule.handles_alert {
                    if p.handles > limit {
                        if self.handles_alerted.insert(id) {
                            let what = format!(
                                "{} ({}) holds {} handles, over the {} its rule allows",
                                p.name,
                                p.pid,
                                fmt::count(p.handles as u64),
                                fmt::count(limit as u64)
                            );
                            alerts.push(self.fire(p, rule, rule.on_handles, what, now));
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
                            let what = format!(
                                "{} ({}) has used over {pct:.0} % of the CPU for {} s",
                                p.name, p.pid, rule.cpu_alert_secs
                            );
                            alerts.push(self.fire(p, rule, rule.on_cpu, what, now));
                        }
                    } else {
                        self.cpu_since.remove(&id);
                        self.cpu_alerted.remove(&id);
                    }
                }
                if let Some(secs) = rule.hang_secs {
                    if p.not_responding {
                        let since = *self.hang_since.entry(id).or_insert(now);
                        if now - since >= Duration::from_secs(secs as u64)
                            && self.hang_alerted.insert(id)
                        {
                            let what =
                                format!("{} ({}) has not responded for {secs} s", p.name, p.pid);
                            alerts.push(self.fire(p, rule, rule.on_hang, what, now));
                        }
                    } else {
                        self.hang_since.remove(&id);
                        self.hang_alerted.remove(&id);
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
        self.hang_since.retain(|(k, _), _| live.contains(k));
        self.hang_alerted.retain(|(k, _)| live.contains(k));
        self.conditioned.retain(|(k, _), _| live.contains(k));
        (actions, alerts)
    }

    /// A watch fired on `p`: what to tell and do. Windows' core processes and
    /// anti-cheat programs are only told about; a dump waits ten minutes
    /// after the rule's last.
    fn fire(
        &mut self,
        p: &ProcessInfo,
        rule: &Rule,
        response: Response,
        what: String,
        now: Instant,
    ) -> Fired {
        let mut response = response;
        let mut note = "";
        let name = p.name.to_lowercase();
        if response != Response::Tell
            && (crate::gamemode::untouchable(&name)
                || crate::platform::CRITICAL_PROCESSES.contains(&name.as_str()))
        {
            response = Response::Tell;
            note = " (left alone: a core or anti-cheat process)";
        }
        if response.dumps() {
            let image = rule.image.trim().to_lowercase();
            if self
                .last_dump
                .get(&image)
                .is_some_and(|&at| now - at < DUMP_EVERY)
            {
                response = response.without_dump();
                note = " (no dump: one was written in the last ten minutes)";
            } else {
                self.last_dump.insert(image, now);
            }
        }
        let text = match response {
            Response::Tell => format!("{what}{note}"),
            r => format!("{what}: {}{note}", r.doing()),
        };
        Fired {
            text,
            target: target(p),
            response,
        }
    }

    /// Anabolic is quitting: what rules' conditions changed goes back.
    pub fn release(&mut self, snap: &Snapshot) -> Vec<Action> {
        let mut out = Vec::new();
        for ((key, _), c) in self.conditioned.drain() {
            if let Some(p) = snap.find(key).filter(|p| p.state != ProcState::Exited) {
                restore(p, c, &mut out);
            }
        }
        out
    }

    /// Follows a rule's condition on one process: applies what waits for
    /// it once it has held long enough, and puts it back once it passes.
    fn condition(
        &mut self,
        id: (ProcKey, usize),
        p: &ProcessInfo,
        rule: &Rule,
        front: Option<&str>,
        now: Instant,
        out: &mut Vec<Action>,
    ) {
        let holds = match rule.when {
            When::Always => return,
            When::Busy { pct, .. } => p.cpu >= pct,
            // Not known what is in front: nothing changes.
            When::Background => match front {
                Some(front) => !front.eq_ignore_ascii_case(&p.name),
                None => return,
            },
        };
        let c = self.conditioned.entry(id).or_default();
        if !c.applied {
            if !holds {
                c.since = None;
                return;
            }
            let since = *c.since.get_or_insert(now);
            let wait = match rule.when {
                When::Busy { secs, .. } => Duration::from_secs(secs as u64),
                _ => AWAY_FOR,
            };
            if now - since >= wait {
                apply(p, rule, c, out);
            }
            return;
        }
        let (calm, wait) = match rule.when {
            When::Busy { pct, .. } => (p.cpu < pct / 2.0, CALM_FOR),
            // Back in front: at once.
            _ => (!holds, Duration::ZERO),
        };
        if !calm {
            c.calm_since = None;
            return;
        }
        let since = *c.calm_since.get_or_insert(now);
        if now - since >= wait
            && let Some(c) = self.conditioned.remove(&id)
        {
            restore(p, c, out);
        }
    }
}

fn target(p: &ProcessInfo) -> Target {
    Target {
        key: p.key,
        name: p.name.clone(),
    }
}

/// Sets what a rule's condition waited for on one process.
fn apply(p: &ProcessInfo, rule: &Rule, c: &mut Conditioned, out: &mut Vec<Action>) {
    c.applied = true;
    c.since = None;
    c.calm_since = None;
    let before = Priority::from_base(p.base_priority);
    if let Some(priority) = rule.priority
        && before != Some(priority.core())
    {
        c.original = before;
        c.set = Some(priority.core());
        out.push(Action::SetPriority(target(p), priority.core()));
    }
    // Efficiency mode takes the priority to idle, and leaving it to normal.
    if rule.efficiency == Some(true) {
        c.original = before;
        c.efficient = true;
        out.push(Action::SetEfficiencyMode(target(p), true));
    }
    if let Some(pct) = rule.cpu_limit_pct {
        let group = format!(
            "{}/{}:{}",
            anabolic_core::limits::rule_group(&rule.image),
            p.key.pid,
            p.key.create_time
        );
        out.push(Action::Limit {
            group: group.clone(),
            targets: vec![target(p)],
            limits: Limits {
                cpu_pct: Some(pct),
                memory: None,
            },
        });
        c.limit = Some(group);
    }
}

/// Puts back what a condition set. A priority goes back only while it is
/// the one set: a change made since stands.
fn restore(p: &ProcessInfo, c: Conditioned, out: &mut Vec<Action>) {
    let now = Priority::from_base(p.base_priority);
    if c.efficient {
        if now == Some(Priority::Idle) {
            out.push(Action::SetEfficiencyMode(target(p), false));
            if let Some(original) = c.original.filter(|&o| o != Priority::Normal) {
                out.push(Action::SetPriority(target(p), original));
            }
        }
    } else if let (Some(original), Some(set)) = (c.original, c.set)
        && now == Some(set)
    {
        out.push(Action::SetPriority(target(p), original));
    }
    if let Some(group) = c.limit {
        out.push(Action::Lift(group));
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
             ones when you save it; a change you make by hand afterwards stands. Its priority, \
             efficiency mode and CPU limit can wait instead until the program is busy or in the \
             background, and go back when that passes. Alerts are checked on every refresh.",
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
        ("Watches", 260.0, Align::Min),
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

/// "then [tell me]": what a watch does when it fires.
fn respond(ui: &mut Ui, id: &str, response: &mut Response, on: bool) {
    ui.add_enabled_ui(on, |ui| {
        ui.label("then");
        egui::ComboBox::from_id_salt(id)
            .selected_text(response.label())
            .width(230.0)
            .show_ui(ui, |ui| {
                for choice in Response::ALL {
                    ui.selectable_value(response, choice, choice.label());
                }
            });
    });
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

                    ui.label("Applies");
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            let busy = matches!(d.rule.when, When::Busy { .. });
                            if ui.selectable_label(d.rule.when == When::Always, "Always").clicked() {
                                d.rule.when = When::Always;
                            }
                            if ui.selectable_label(busy, "While it is busy").clicked() && !busy {
                                d.rule.when = When::Busy { pct: 20.0, secs: 10 };
                            }
                            if ui
                                .selectable_label(d.rule.when == When::Background, "While it is in the background")
                                .clicked()
                            {
                                d.rule.when = When::Background;
                            }
                        });
                        let note = match &mut d.rule.when {
                            When::Always => "From the moment each of its processes starts.".to_string(),
                            When::Busy { pct, secs } => {
                                ui.horizontal(|ui| {
                                    ui.label("over");
                                    ui.add(egui::DragValue::new(pct).range(1.0..=100.0).suffix(" %"));
                                    ui.label("of the CPU for");
                                    ui.add(egui::DragValue::new(secs).range(1..=3600).suffix(" s"));
                                });
                                format!(
                                    "Put back once it has stayed under {:.0} % for 10 s. Each of its \
                                     processes is judged by itself.",
                                    *pct / 2.0
                                )
                            }
                            When::Background => "While none of its windows is in front; put back as one \
                                                 comes to the front."
                                .to_string(),
                        };
                        ui.label(
                            RichText::new(format!(
                                "{note} Priority, efficiency mode and the CPU limit follow this; cores, the \
                                 memory limit and alerts always apply."
                            ))
                            .size(theme::META)
                            .color(theme::TEXT_WEAK),
                        );
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

                    let can_limit = anabolic_core::limits::available();
                    ui.label("CPU limit");
                    ui.horizontal(|ui| {
                        let mut on = d.rule.cpu_limit_pct.is_some();
                        ui.add_enabled(can_limit.is_ok(), egui::Checkbox::new(&mut on, "At most"))
                            .on_disabled_hover_text(can_limit.clone().err().unwrap_or_default());
                        let mut pct = d.rule.cpu_limit_pct.unwrap_or(25.0);
                        ui.add_enabled(
                            on,
                            egui::DragValue::new(&mut pct)
                                .range(1.0..=99.0)
                                .speed(0.5)
                                .max_decimals(1)
                                .suffix(" %"),
                        );
                        d.rule.cpu_limit_pct = on.then_some(pct);
                        ui.label(
                            RichText::new(
                                "of the whole machine, all its processes together: Windows holds it \
                                 there however busy the machine is",
                            )
                            .size(theme::META)
                            .color(theme::TEXT_WEAK),
                        );
                    });
                    ui.end_row();

                    ui.label("Memory limit");
                    ui.horizontal(|ui| {
                        let mut on = d.rule.memory_limit_gb.is_some();
                        ui.add_enabled(can_limit.is_ok(), egui::Checkbox::new(&mut on, "At most"))
                            .on_disabled_hover_text(can_limit.clone().err().unwrap_or_default());
                        let mut gb = d.rule.memory_limit_gb.unwrap_or(4.0);
                        ui.add_enabled(
                            on,
                            egui::DragValue::new(&mut gb)
                                .range(0.1..=1024.0)
                                .speed(0.05)
                                .max_decimals(2)
                                .suffix(" GB"),
                        );
                        d.rule.memory_limit_gb = on.then_some(gb);
                        ui.label(
                            RichText::new(
                                "of private memory, all its processes together: past it their requests \
                                 for more fail, and most programs then stop with an error",
                            )
                            .size(theme::META)
                            .color(theme::TEXT_WEAK),
                        );
                    });
                    ui.end_row();

                    ui.label("Game");
                    ui.checkbox(&mut d.rule.game, "It is a game: game mode starts while it is in front")
                        .on_hover_text(
                            "Game mode notices games that fill the screen by itself; this is for \
                             games played in a window.",
                        );
                    ui.end_row();

                    ui.label("Watches");
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
                            respond(ui, "rule-on-memory", &mut d.rule.on_memory, on);
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
                            respond(ui, "rule-on-cpu", &mut d.rule.on_cpu, on);
                            d.rule.cpu_alert_pct = on.then_some(pct);
                        });
                        ui.horizontal(|ui| {
                            let mut on = d.rule.handles_alert.is_some();
                            ui.checkbox(&mut on, "Handles pass")
                                .on_hover_text(
                                    "A program that keeps opening handles and never closes them \
                                     slows itself and the system down over time.",
                                );
                            let mut n = d.rule.handles_alert.unwrap_or(50_000);
                            ui.add_enabled(
                                on,
                                egui::DragValue::new(&mut n)
                                    .range(1_000..=10_000_000)
                                    .speed(100.0),
                            );
                            respond(ui, "rule-on-handles", &mut d.rule.on_handles, on);
                            d.rule.handles_alert = on.then_some(n);
                        });
                        ui.horizontal(|ui| {
                            let mut on = d.rule.hang_secs.is_some();
                            ui.add_enabled(
                                cfg!(windows),
                                egui::Checkbox::new(&mut on, "A window stops responding for"),
                            )
                            .on_disabled_hover_text(
                                "Anabolic cannot tell on Linux when a window stops answering.",
                            );
                            let mut secs = d.rule.hang_secs.unwrap_or(30);
                            ui.add_enabled(
                                on,
                                egui::DragValue::new(&mut secs).range(5..=3600).suffix(" s"),
                            );
                            respond(ui, "rule-on-hang", &mut d.rule.on_hang, on);
                            d.rule.hang_secs = on.then_some(secs);
                        });
                    });
                    ui.end_row();
                });
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "Priority, cores, efficiency mode and limits are set by opening the process. \
                     Anti-cheat protected games may refuse this or treat it as suspicious; the limits \
                     leave Windows' core processes and anti-cheat programs alone.",
                )
                .size(theme::META)
                .color(theme::ELEVATED_TEXT),
            );
            let r = &d.rule;
            let acts = [r.on_memory, r.on_cpu, r.on_handles, r.on_hang];
            if acts.iter().any(|a| *a != Response::Tell) {
                ui.label(
                    RichText::new(
                        "A dump opens the process and reads its memory; at most one is written \
                         every ten minutes per rule. Ending a process loses what it has not \
                         saved. Windows' core processes and anti-cheat programs are only told \
                         about.",
                    )
                    .size(theme::META)
                    .color(theme::ELEVATED_TEXT),
                );
            }
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

    /// What the actions do, in a few words.
    fn said(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .map(|a| match a {
                Action::SetPriority(_, p) => format!("priority {}", p.label()),
                Action::SetEfficiencyMode(_, on) => format!("efficiency {on}"),
                Action::Limit { group, limits, .. } => {
                    format!("limit {group} {:?} {:?}", limits.cpu_pct, limits.memory)
                }
                Action::Lift(group) => format!("lift {group}"),
                _ => "something else".into(),
            })
            .collect()
    }

    /// The one process at `cpu` % with base priority `base`.
    fn at(seq: u64, cpu: f32, base: i32) -> Snapshot {
        let mut snap = snapshot(seq, cpu, 0.5);
        snap.processes[0].base_priority = base;
        snap
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn busy_rule() -> Vec<Rule> {
        vec![Rule {
            image: "game.exe".into(),
            priority: Some(RulePriority::BelowNormal),
            cpu_limit_pct: Some(10.0),
            when: When::Busy { pct: 20.0, secs: 5 },
            ..Rule::default()
        }]
    }

    #[test]
    fn busy_long_enough_it_is_held_back_and_put_back_once_calm() {
        let rules = busy_rule();
        let mut e = Engine::default();
        let t0 = Instant::now();
        // Busy, not yet for long enough.
        assert!(e.step_at(&rules, &at(2, 50.0, 8), None, t0).0.is_empty());
        assert!(
            e.step_at(&rules, &at(3, 50.0, 8), None, t0 + secs(4))
                .0
                .is_empty()
        );
        let (a, _) = e.step_at(&rules, &at(4, 50.0, 8), None, t0 + secs(5));
        assert_eq!(
            said(&a),
            [
                "priority Below normal",
                "limit rule:game.exe/100:5 Some(10.0) None"
            ]
        );
        // Calm, not yet for long enough; then for long enough.
        assert!(
            e.step_at(&rules, &at(5, 5.0, 6), None, t0 + secs(6))
                .0
                .is_empty()
        );
        let (a, _) = e.step_at(&rules, &at(6, 5.0, 6), None, t0 + secs(16));
        assert_eq!(said(&a), ["priority Normal", "lift rule:game.exe/100:5"]);
        // Busy again: held back again, after the wait.
        assert!(
            e.step_at(&rules, &at(7, 50.0, 8), None, t0 + secs(17))
                .0
                .is_empty()
        );
        assert_eq!(
            e.step_at(&rules, &at(8, 50.0, 8), None, t0 + secs(22))
                .0
                .len(),
            2
        );
    }

    #[test]
    fn quitting_puts_back_what_a_condition_set() {
        let rules = busy_rule();
        let mut e = Engine::default();
        let t0 = Instant::now();
        e.step_at(&rules, &at(2, 50.0, 8), None, t0);
        assert_eq!(
            e.step_at(&rules, &at(3, 50.0, 8), None, t0 + secs(5))
                .0
                .len(),
            2
        );
        let back = e.release(&at(4, 50.0, 6));
        assert_eq!(said(&back), ["priority Normal", "lift rule:game.exe/100:5"]);
        // Once.
        assert!(e.release(&at(5, 50.0, 6)).is_empty());
    }

    #[test]
    fn a_priority_changed_by_hand_meanwhile_stands() {
        let rules = busy_rule();
        let mut e = Engine::default();
        let t0 = Instant::now();
        e.step_at(&rules, &at(2, 50.0, 8), None, t0);
        assert_eq!(
            e.step_at(&rules, &at(3, 50.0, 8), None, t0 + secs(5))
                .0
                .len(),
            2
        );
        // Set to high by hand, then calm: only the limit goes.
        e.step_at(&rules, &at(4, 5.0, 13), None, t0 + secs(6));
        let (a, _) = e.step_at(&rules, &at(5, 5.0, 13), None, t0 + secs(16));
        assert_eq!(said(&a), ["lift rule:game.exe/100:5"]);
    }

    #[test]
    fn in_the_background_it_waits_and_comes_back_at_once_in_front() {
        let rules = vec![Rule {
            image: "game.exe".into(),
            efficiency: Some(true),
            when: When::Background,
            ..Rule::default()
        }];
        let mut e = Engine::default();
        let t0 = Instant::now();
        // Not known what is in front, or it is: nothing.
        assert!(e.step_at(&rules, &at(2, 0.0, 8), None, t0).0.is_empty());
        assert!(
            e.step_at(&rules, &at(3, 0.0, 8), Some("GAME.exe"), t0)
                .0
                .is_empty()
        );
        // Behind another program long enough.
        assert!(
            e.step_at(&rules, &at(4, 0.0, 8), Some("other.exe"), t0 + secs(1))
                .0
                .is_empty()
        );
        let (a, _) = e.step_at(&rules, &at(5, 0.0, 8), Some("other.exe"), t0 + secs(3));
        assert_eq!(said(&a), ["efficiency true"]);
        // In front again: put back at once, its priority with it.
        let (a, _) = e.step_at(&rules, &at(6, 0.0, 4), Some("game.exe"), t0 + secs(4));
        assert_eq!(said(&a), ["efficiency false"]);
    }

    /// Processes of these names, those named in `hung` not responding.
    fn hanging(seq: u64, names: &[(u32, &str)], hung: &[u32]) -> Snapshot {
        let mut snap = snapshot(seq, 0.0, 0.5);
        snap.processes = names
            .iter()
            .map(|&(pid, name)| {
                let mut p = ProcessInfo::blank(
                    ProcKey {
                        pid,
                        create_time: pid as i64,
                    },
                    4,
                    Arc::from(name),
                );
                p.not_responding = hung.contains(&pid);
                p
            })
            .collect();
        snap
    }

    #[test]
    fn a_hang_long_enough_writes_a_dump_then_ends_it() {
        let rules = vec![Rule {
            image: "app.exe".into(),
            hang_secs: Some(30),
            on_hang: Response::DumpThenEnd(Dump::Small),
            ..Rule::default()
        }];
        let mut e = Engine::default();
        let t0 = Instant::now();
        let apps = [(100, "app.exe"), (200, "app.exe")];
        assert!(
            e.step_at(&rules, &hanging(2, &apps, &[100]), None, t0)
                .1
                .is_empty()
        );
        let (_, fired) = e.step_at(&rules, &hanging(3, &apps, &[100]), None, t0 + secs(30));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].target.key.pid, 100);
        assert_eq!(fired[0].response, Response::DumpThenEnd(Dump::Small));
        assert!(
            fired[0]
                .text
                .ends_with("writing a small dump, then ending it")
        );
        // Once only, until it answers again.
        assert!(
            e.step_at(&rules, &hanging(4, &apps, &[100]), None, t0 + secs(40))
                .1
                .is_empty()
        );
        // The other one hangs too, soon after: ended, no second dump yet.
        e.step_at(&rules, &hanging(5, &apps, &[200]), None, t0 + secs(41));
        let (_, fired) = e.step_at(&rules, &hanging(6, &apps, &[200]), None, t0 + secs(71));
        assert_eq!(fired[0].response, Response::End);
        assert!(fired[0].text.contains("in the last ten minutes"));
        // Ten minutes on, a dump again.
        e.step_at(&rules, &hanging(7, &apps, &[]), None, t0 + secs(700));
        e.step_at(&rules, &hanging(8, &apps, &[100]), None, t0 + secs(701));
        let (_, fired) = e.step_at(&rules, &hanging(9, &apps, &[100]), None, t0 + secs(731));
        assert_eq!(fired[0].response, Response::DumpThenEnd(Dump::Small));
    }

    #[test]
    fn anti_cheat_is_only_told_about() {
        let rules = vec![Rule {
            image: "EasyAntiCheat.exe".into(),
            hang_secs: Some(5),
            on_hang: Response::End,
            ..Rule::default()
        }];
        let mut e = Engine::default();
        let t0 = Instant::now();
        let eac = [(300, "EasyAntiCheat.exe")];
        e.step_at(&rules, &hanging(2, &eac, &[300]), None, t0);
        let (_, fired) = e.step_at(&rules, &hanging(3, &eac, &[300]), None, t0 + secs(5));
        assert_eq!(fired[0].response, Response::Tell);
        assert!(fired[0].text.contains("left alone"));
    }

    #[test]
    fn a_rules_limit_holds_the_program_and_goes_with_the_rule() {
        let rules = vec![Rule {
            image: "game.exe".into(),
            memory_limit_gb: Some(2.0),
            ..Rule::default()
        }];
        let mut e = Engine::default();
        let t0 = Instant::now();
        let (a, _) = e.step_at(&rules, &at(2, 0.0, 8), None, t0);
        assert_eq!(
            said(&a),
            [format!("limit rule:game.exe None Some({})", 2u64 << 30)]
        );
        assert!(e.step_at(&rules, &at(3, 0.0, 8), None, t0).0.is_empty());
        // The rule goes: so does its limit.
        let (a, _) = e.step_at(&[], &at(4, 0.0, 8), None, t0);
        assert_eq!(said(&a), ["lift rule:game.exe"]);
    }
}
