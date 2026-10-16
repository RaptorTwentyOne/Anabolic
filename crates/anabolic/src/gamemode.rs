//! Game mode: while a game is in front, background programs that take a
//! real share of the processor are lowered to below normal priority, and
//! put back once they calm down or the game ends (Process Lasso calls this
//! ProBalance). Optionally the power plan goes to High performance while
//! the game runs. The game, its helpers, Windows' own core processes and
//! anti-cheat software are never touched.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anabolic_core::actions::{Action, Priority, Target};
use anabolic_core::foreground::Front;
use anabolic_core::{ProcKey, ProcState, ProcessInfo, Snapshot};
use eframe::egui::{self, RichText, Ui};
use serde::{Deserialize, Serialize};

use crate::rules::Rule;
use crate::{theme, widgets};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameModeSettings {
    pub enabled: bool,
    /// Lower background programs that use the processor.
    pub restrain: bool,
    /// …when one takes more than this share of the machine, percent.
    pub cpu_pct: f32,
    /// High performance power plan while a game runs.
    pub power: bool,
    /// The plan game mode replaced, to put back when the game ends (or on
    /// the next start, if Anabolic did not get to).
    pub replaced_plan: Option<String>,
}

impl Default for GameModeSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            restrain: true,
            cpu_pct: 10.0,
            power: false,
            replaced_plan: None,
        }
    }
}

/// The game must have left the front this long before game mode ends: a
/// look at another window (Anabolic, say) is not the end of a game.
const GRACE: Duration = Duration::from_secs(30);
/// A program must keep up its share this long to be lowered…
const HOG_FOR: Duration = Duration::from_secs(2);
/// …and stay under half of it this long to be put back.
const CALM_FOR: Duration = Duration::from_secs(10);
const LOG: usize = 40;

/// Never lowered: Windows' core, the shell, audio (lowering it makes sound
/// crackle), and Anabolic.
const NEVER: [&str; 19] = [
    "system",
    "registry",
    "secure system",
    "memory compression",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "lsaiso.exe",
    "dwm.exe",
    "audiodg.exe",
    "fontdrvhost.exe",
    "explorer.exe",
    "sihost.exe",
    "ctfmon.exe",
    "anabolic.exe",
    "anabolic-launch.exe",
];

/// Parts of the names of anti-cheat programs: left alone entirely.
const ANTI_CHEAT: [&str; 18] = [
    "anticheat",
    "anti-cheat",
    "easyanticheat",
    "eac_",
    "beservice",
    "battleye",
    "vgc",
    "vgtray",
    "faceit",
    "xigncode",
    "xhunter",
    "xcorona",
    "gamemon",
    "gameguard",
    "nprotect",
    "mhyprot",
    "sguard",
    "ricochet",
];

pub fn untouchable(name: &str) -> bool {
    let name = name.to_lowercase();
    NEVER.contains(&name.as_str()) || ANTI_CHEAT.iter().any(|a| name.contains(a))
}

pub struct Game {
    pub key: ProcKey,
    pub name: Arc<str>,
    pub since: SystemTime,
    last_seen: Instant,
}

struct Restrained {
    name: Arc<str>,
    original: Priority,
    /// Seen at below normal since: the change took.
    confirmed: bool,
    checks: u8,
    calm_since: Option<Instant>,
}

#[derive(Default)]
pub struct GameMode {
    pub game: Option<Game>,
    hog_since: HashMap<ProcKey, Instant>,
    restrained: HashMap<ProcKey, Restrained>,
    /// Refused the change once (protected): not asked again.
    refused: HashSet<ProcKey>,
    pub log: VecDeque<(SystemTime, String)>,
    /// The power plan was asked for this game.
    power_asked: bool,
    /// Putting back a plan left over from an earlier run was asked.
    restore_asked: bool,
    last_seq: u64,
}

fn base(p: Priority) -> i32 {
    match p {
        Priority::Idle => 4,
        Priority::BelowNormal => 6,
        Priority::Normal => 8,
        Priority::AboveNormal => 10,
        Priority::High => 13,
        Priority::Realtime => 24,
    }
}

impl GameMode {
    pub fn note(&mut self, text: impl Into<String>) {
        self.log.push_back((SystemTime::now(), text.into()));
        while self.log.len() > LOG {
            self.log.pop_front();
        }
    }

    /// Programs lowered now.
    pub fn lowered(&self) -> usize {
        self.restrained.values().filter(|r| r.confirmed).count()
    }

    /// The game in front, if any: a window that fills its monitor, or a
    /// program a rule calls a game.
    fn detect<'a>(
        snap: &'a Snapshot,
        front: Option<Front>,
        rules: &[Rule],
    ) -> Option<&'a ProcessInfo> {
        let front = front?;
        let p = snap
            .processes
            .iter()
            .find(|p| p.pid == front.pid && p.state != ProcState::Exited)?;
        if p.pid == snap.own_pid || untouchable(&p.name) {
            return None;
        }
        let marked = rules.iter().any(|r| r.game && r.matches(&p.name));
        (marked || front.fullscreen).then_some(p)
    }

    fn put_back(&mut self, key: ProcKey, p: &ProcessInfo, out: &mut Vec<Action>) {
        if let Some(r) = self.restrained.remove(&key)
            && r.confirmed
            && p.base_priority == base(Priority::BelowNormal)
        {
            out.push(Action::Silent(Box::new(Action::SetPriority(
                Target {
                    key,
                    name: r.name.clone(),
                },
                r.original,
            ))));
        }
    }

    /// Once per snapshot: what to change.
    pub fn step(
        &mut self,
        s: &mut GameModeSettings,
        snap: &Snapshot,
        rules: &[Rule],
    ) -> Vec<Action> {
        if snap.seq == self.last_seq || snap.seq < 2 {
            return Vec::new();
        }
        let front = if s.enabled {
            anabolic_core::foreground::front()
        } else {
            None
        };
        self.step_at(s, snap, rules, front, Instant::now())
    }

    fn step_at(
        &mut self,
        s: &mut GameModeSettings,
        snap: &Snapshot,
        rules: &[Rule],
        front: Option<Front>,
        now: Instant,
    ) -> Vec<Action> {
        let mut out = Vec::new();
        self.last_seq = snap.seq;
        if let Some(p) = Self::detect(snap, front, rules) {
            if let Some(g) = self.game.as_mut().filter(|g| g.key == p.key) {
                g.last_seen = now;
            } else {
                self.note(format!("{} is in front: game mode on", p.name));
                self.game = Some(Game {
                    key: p.key,
                    name: p.name.clone(),
                    since: SystemTime::now(),
                    last_seen: now,
                });
            }
        }
        // Over: switched off, the game exited, or away long enough.
        let over = self.game.as_ref().is_some_and(|g| {
            !s.enabled
                || snap
                    .find(g.key)
                    .is_none_or(|p| p.state == ProcState::Exited)
                || now - g.last_seen > GRACE
        });
        if over && let Some(g) = self.game.take() {
            let lowered: Vec<ProcKey> = self.restrained.keys().copied().collect();
            let mut back = 0;
            for key in lowered {
                if let Some(p) = snap.find(key) {
                    let confirmed = self.restrained.get(&key).is_some_and(|r| r.confirmed);
                    self.put_back(key, p, &mut out);
                    back += confirmed as usize;
                }
            }
            self.restrained.clear();
            self.hog_since.clear();
            self.refused.clear();
            self.note(if back > 0 {
                format!(
                    "{} has gone: game mode off, {back} programs put back",
                    g.name
                )
            } else {
                format!("{} has gone: game mode off", g.name)
            });
            self.power_asked = false;
        }

        // The power plan: High performance while playing, the old one after.
        if let Some(previous) = s.replaced_plan.clone() {
            if self.game.is_none() && !self.restore_asked {
                self.restore_asked = true;
                out.push(Action::GamePower {
                    on: false,
                    previous: Some(previous),
                });
            }
        } else if self.game.is_some() && s.power && !self.power_asked {
            self.power_asked = true;
            self.restore_asked = false;
            out.push(Action::GamePower {
                on: true,
                previous: None,
            });
        }

        let Some(game) = &self.game else {
            return out;
        };
        let game_pid = game.key.pid;
        let ruled = |name: &str| {
            rules
                .iter()
                .any(|r| r.priority.is_some() && r.matches(name))
        };
        for p in snap.processes.iter().filter(|p| {
            p.state != ProcState::Exited
                && !p.is_pseudo()
                && p.pid != game_pid
                && p.ppid != game_pid
                && p.pid != snap.own_pid
        }) {
            let candidate = s.restrain
                && p.cpu >= s.cpu_pct
                && p.base_priority >= base(Priority::Normal)
                && p.base_priority < base(Priority::Realtime)
                && Priority::from_base(p.base_priority).is_some()
                && !untouchable(&p.name)
                && !ruled(&p.name)
                && !self.refused.contains(&p.key)
                && !self.restrained.contains_key(&p.key);
            if !candidate {
                self.hog_since.remove(&p.key);
                continue;
            }
            let since = *self.hog_since.entry(p.key).or_insert(now);
            if now - since < HOG_FOR {
                continue;
            }
            self.hog_since.remove(&p.key);
            let original = Priority::from_base(p.base_priority).unwrap_or(Priority::Normal);
            out.push(Action::Silent(Box::new(Action::SetPriority(
                Target {
                    key: p.key,
                    name: p.name.clone(),
                },
                Priority::BelowNormal,
            ))));
            self.restrained.insert(
                p.key,
                Restrained {
                    name: p.name.clone(),
                    original,
                    confirmed: false,
                    checks: 0,
                    calm_since: None,
                },
            );
        }

        // Follow the ones lowered: did it take, did they calm down?
        let keys: Vec<ProcKey> = self.restrained.keys().copied().collect();
        for key in keys {
            let Some(p) = snap.find(key).filter(|p| p.state != ProcState::Exited) else {
                self.restrained.remove(&key);
                continue;
            };
            let below = p.base_priority == base(Priority::BelowNormal);
            let Some(r) = self.restrained.get_mut(&key) else {
                continue;
            };
            if !r.confirmed {
                r.checks += 1;
                if below {
                    r.confirmed = true;
                    let text = format!(
                        "Lowered {} ({:.0} % CPU) to below normal, from {}",
                        p.name,
                        p.cpu,
                        r.original.label().to_lowercase()
                    );
                    self.note(text);
                } else if r.checks >= 3 {
                    let text = format!("{} would not be lowered (protected); left alone", p.name);
                    self.restrained.remove(&key);
                    self.refused.insert(key);
                    self.note(text);
                }
                continue;
            }
            if !below {
                // Someone else set its priority since: theirs stands.
                self.restrained.remove(&key);
                continue;
            }
            if p.cpu < s.cpu_pct / 2.0 {
                let calm = *r.calm_since.get_or_insert(now);
                if now - calm >= CALM_FOR {
                    let text = format!(
                        "{} calmed down: put back to {}",
                        p.name,
                        r.original.label().to_lowercase()
                    );
                    self.put_back(key, p, &mut out);
                    self.note(text);
                }
            } else {
                r.calm_since = None;
            }
        }
        out
    }
}

/// The game mode card at the top of the Rules page.
pub fn card(ui: &mut Ui, s: &mut GameModeSettings, mode: &GameMode) {
    egui::Frame::new()
        .fill(theme::RAISED)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(theme::RADIUS_PANEL)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(widgets::strong("Game mode", theme::SUBTITLE));
                ui.add_space(8.0);
                ui.checkbox(&mut s.enabled, "On");
                if s.enabled {
                    ui.add_space(12.0);
                    let status = match &mode.game {
                        Some(g) => format!(
                            "{} in front since {}  ·  {} lowered",
                            g.name,
                            crate::fmt::clock(g.since, false),
                            mode.lowered()
                        ),
                        None => "No game in front".into(),
                    };
                    let color = if mode.game.is_some() {
                        theme::GOOD
                    } else {
                        theme::TEXT_WEAK
                    };
                    ui.label(RichText::new(status).color(color));
                }
            });
            ui.label(
                RichText::new(
                    "While a game is in front (its window fills the screen, or a rule says it is \
                     a game), programs in the background that take a real share of the processor \
                     run at below normal priority until they calm down or the game ends. The game \
                     itself, Windows' core processes, audio and anti-cheat software are never \
                     touched.",
                )
                .size(theme::META)
                .color(theme::TEXT_WEAK),
            );
            ui.add_space(4.0);
            ui.add_enabled_ui(s.enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut s.restrain, "Lower background programs using over");
                    ui.add_enabled(
                        s.restrain,
                        egui::DragValue::new(&mut s.cpu_pct)
                            .range(2.0..=50.0)
                            .speed(0.5)
                            .suffix(" %"),
                    );
                    ui.label("of the machine");
                });
                ui.checkbox(
                    &mut s.power,
                    "Switch to the High performance power plan while playing",
                )
                .on_hover_text(
                    "Puts the plan you use now back when the game ends, unless you picked \
                         another one meanwhile.",
                );
            });
            if !mode.log.is_empty() {
                ui.add_space(4.0);
                for (at, text) in mode.log.iter().rev().take(6) {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(crate::fmt::clock(*at, false))
                                .size(theme::META)
                                .color(theme::TEXT_DIM),
                        );
                        ui.label(
                            RichText::new(text)
                                .size(theme::META)
                                .color(theme::TEXT_WEAK),
                        );
                    });
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use anabolic_core::history::History;

    fn process(pid: u32, name: &str, cpu: f32, base: i32) -> ProcessInfo {
        let mut p = ProcessInfo::blank(
            ProcKey {
                pid,
                create_time: pid as i64,
            },
            1,
            name.into(),
        );
        p.cpu = cpu;
        p.base_priority = base;
        p
    }

    fn snapshot(seq: u64, processes: Vec<ProcessInfo>) -> Snapshot {
        let mut snap = Snapshot::empty(1, Arc::new(History::new(4, 1, &[])));
        snap.seq = seq;
        snap.own_pid = 9;
        snap.processes = processes;
        snap
    }

    fn lowered(actions: &[Action]) -> Vec<(String, Priority)> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Silent(inner) => match &**inner {
                    Action::SetPriority(t, p) => Some((t.name.to_string(), *p)),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    #[test]
    fn lowers_background_hogs_while_a_game_is_in_front_and_puts_them_back() {
        let mut s = GameModeSettings {
            enabled: true,
            ..Default::default()
        };
        let mut mode = GameMode::default();
        let front = Some(Front {
            pid: 100,
            fullscreen: true,
        });
        let t0 = Instant::now();
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        let busy = |seq, hog_cpu, hog_base| {
            snapshot(
                seq,
                vec![
                    process(100, "game.exe", 60.0, 8),
                    process(200, "indexer.exe", hog_cpu, hog_base),
                    process(300, "audiodg.exe", 30.0, 8),
                    process(400, "quiet.exe", 1.0, 8),
                ],
            )
        };
        // A game in front; the hog has to keep it up for two seconds.
        let a = mode.step_at(&mut s, &busy(2, 30.0, 8), &[], front, at(0));
        assert!(lowered(&a).is_empty());
        assert!(mode.game.is_some());
        let a = mode.step_at(&mut s, &busy(3, 30.0, 8), &[], front, at(3));
        assert_eq!(lowered(&a), [("indexer.exe".into(), Priority::BelowNormal)]);
        // It took (below normal is base 6), then it calms down for 10 s.
        mode.step_at(&mut s, &busy(4, 30.0, 6), &[], front, at(4));
        assert_eq!(mode.lowered(), 1);
        mode.step_at(&mut s, &busy(5, 1.0, 6), &[], front, at(5));
        let a = mode.step_at(&mut s, &busy(6, 1.0, 6), &[], front, at(16));
        assert_eq!(lowered(&a), [("indexer.exe".into(), Priority::Normal)]);
        assert_eq!(mode.lowered(), 0);
    }

    #[test]
    fn the_end_of_the_game_puts_everything_back() {
        let mut s = GameModeSettings {
            enabled: true,
            ..Default::default()
        };
        let mut mode = GameMode::default();
        let front = Some(Front {
            pid: 100,
            fullscreen: true,
        });
        let t0 = Instant::now();
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        let with_game = |seq, base| {
            snapshot(
                seq,
                vec![
                    process(100, "game.exe", 60.0, 8),
                    process(200, "indexer.exe", 30.0, base),
                ],
            )
        };
        mode.step_at(&mut s, &with_game(2, 8), &[], front, at(0));
        mode.step_at(&mut s, &with_game(3, 8), &[], front, at(3));
        mode.step_at(&mut s, &with_game(4, 6), &[], front, at(4));
        assert_eq!(mode.lowered(), 1);
        // The game exits.
        let gone = snapshot(5, vec![process(200, "indexer.exe", 30.0, 6)]);
        let a = mode.step_at(&mut s, &gone, &[], None, at(5));
        assert_eq!(lowered(&a), [("indexer.exe".into(), Priority::Normal)]);
        assert!(mode.game.is_none());
    }

    #[test]
    fn a_window_that_does_not_fill_the_screen_is_not_a_game_unless_a_rule_says() {
        let mut s = GameModeSettings {
            enabled: true,
            ..Default::default()
        };
        let windowed = Some(Front {
            pid: 100,
            fullscreen: false,
        });
        let snap = snapshot(2, vec![process(100, "game.exe", 60.0, 8)]);
        let mut mode = GameMode::default();
        mode.step_at(&mut s, &snap, &[], windowed, Instant::now());
        assert!(mode.game.is_none());
        let rule = Rule {
            image: "game.exe".into(),
            game: true,
            ..Default::default()
        };
        let mut mode = GameMode::default();
        mode.step_at(&mut s, &snap, &[rule], windowed, Instant::now());
        assert!(mode.game.is_some());
    }

    #[test]
    fn leaves_the_core_and_anti_cheat_alone() {
        assert!(untouchable("csrss.exe"));
        assert!(untouchable("AudioDG.exe"));
        assert!(untouchable("EasyAntiCheat_EOS.exe"));
        assert!(untouchable("BEService.exe"));
        assert!(!untouchable("chrome.exe"));
        assert!(!untouchable("avp.exe"));
    }
}
