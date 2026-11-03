// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Limits on what a program may use of the processor and of memory, kept
//! by the system rather than asked for: on Windows a job object, whose CPU
//! cap the scheduler holds to whatever the program's priority. A group (a
//! rule's program, or one process) shares one limit, and what its
//! processes start joins it. Limits last while Anabolic runs: it lifts
//! them as it quits, and as it fails. Ended by force, it cannot: then they
//! last until those processes exit.
//!
//! Who is held, and to what, is kept here, whoever asked (the window, a
//! rule, an AI assistant), so the window shows them all.

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::actions::Target;
use crate::model::ProcKey;

#[cfg_attr(windows, path = "windows/limits.rs")]
#[cfg_attr(target_os = "linux", path = "linux/limits.rs")]
mod imp;
pub use imp::available;

/// What a group may use.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Limits {
    /// A share of the whole machine, percent, as the CPU column counts it.
    pub cpu_pct: Option<f32>,
    /// Private memory of all its processes together, bytes. Past it their
    /// requests for more fail.
    pub memory: Option<u64>,
}

impl Limits {
    pub fn is_empty(&self) -> bool {
        self.cpu_pct.is_none() && self.memory.is_none()
    }
}

/// What a limit, or lifting one, came to.
#[derive(Clone, Debug)]
pub struct Applied {
    /// The group: "rule:chrome.exe", "process:1234:…".
    pub group: String,
    /// What it holds now; empty when lifted.
    pub limits: Limits,
    /// What it held before; None when it held nothing.
    pub before: Option<Limits>,
    /// The processes in it now (asked for and already there).
    pub joined: Vec<ProcKey>,
    /// Those that could not be put in it, and why.
    pub failed: Vec<(Target, String)>,
    /// The limit itself could not be set.
    pub error: Option<String>,
}

/// A limit held now.
#[derive(Clone, Debug, PartialEq)]
pub struct Held {
    pub group: String,
    pub limits: Limits,
    pub members: Vec<ProcKey>,
}

/// The group of a single process, its children with it.
pub fn process_group(key: ProcKey) -> String {
    format!("process:{}:{}", key.pid, key.create_time)
}

/// The group of a rule's program.
pub fn rule_group(image: &str) -> String {
    format!("rule:{}", image.trim().to_lowercase())
}

/// The group of a rule's limit on one of its processes (a condition's).
pub fn rule_process_group(image: &str, key: ProcKey) -> String {
    format!("{}/{}:{}", rule_group(image), key.pid, key.create_time)
}

/// A group of one process: it goes with the process.
fn of_one(group: &str) -> bool {
    group.starts_with("process:") || group.contains('/')
}

type Groups = BTreeMap<String, (Limits, HashSet<ProcKey>)>;

static HELD: Mutex<Groups> = Mutex::new(BTreeMap::new());
/// Counts changes to what is held.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn groups() -> std::sync::MutexGuard<'static, Groups> {
    HELD.lock().unwrap_or_else(|e| e.into_inner())
}

/// Puts `targets` in the group and holds it to `limits`; those it holds
/// already stay.
pub fn limit(group: &str, targets: &[Target], limits: Limits) -> Applied {
    let before = groups().get(group).map(|(l, _)| *l);
    let mut applied = imp::limit(group, targets, limits);
    applied.before = before;
    if applied.error.is_none() {
        let mut held = groups();
        let (held_limits, members) = held.entry(group.to_string()).or_default();
        *held_limits = limits;
        members.extend(applied.joined.iter().copied());
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
    applied
}

/// Stops limiting the group.
pub fn lift(group: &str) -> Applied {
    let before = groups().remove(group).map(|(l, _)| l);
    if before.is_some() {
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
    let mut applied = imp::lift(group);
    applied.before = before;
    applied
}

/// Lifts every limit: Anabolic is quitting, or failing. Not waited for
/// if this thread holds the record (it failed on it).
pub fn lift_all() {
    imp::lift_all();
    if let Ok(mut held) = HELD.try_lock() {
        held.clear();
    }
}

/// The limits held now.
pub fn held() -> Vec<Held> {
    groups()
        .iter()
        .map(|(group, (limits, members))| Held {
            group: group.clone(),
            limits: *limits,
            members: members.iter().copied().collect(),
        })
        .collect()
}

/// Changes with every change to what is held.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// Forgets processes that are gone, and lets go of the groups of one
/// process left empty.
pub fn forget_gone(alive: impl Fn(ProcKey) -> bool) {
    let mut empty = Vec::new();
    {
        let mut held = groups();
        for (group, (_, members)) in held.iter_mut() {
            let before = members.len();
            members.retain(|&k| alive(k));
            if members.len() != before {
                GENERATION.fetch_add(1, Ordering::Relaxed);
            }
            if members.is_empty() && of_one(group) {
                empty.push(group.clone());
            }
        }
    }
    for group in empty {
        lift(&group);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_of_one_process_are_told_apart() {
        let key = ProcKey {
            pid: 7,
            create_time: 9,
        };
        assert!(of_one(&process_group(key)));
        assert!(of_one(&rule_process_group("App.exe", key)));
        assert!(!of_one(&rule_group("App.exe")));
        assert_eq!(rule_process_group(" App.exe", key), "rule:app.exe/7:9");
    }
}
