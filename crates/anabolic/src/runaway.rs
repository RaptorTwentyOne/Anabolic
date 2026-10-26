// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Runaway programs: one program running in dozens or hundreds of copies
//! at once, usually a script or a build gone wrong. Copies are counted per starter:
//! up the parents, past copies that start more copies, to the first other
//! program. Browsers, which keep a process per site, and the system's own
//! service hosts are many processes by design and are left out.

use std::collections::HashMap;
use std::sync::Arc;

use anabolic_core::{ProcKey, ProcState, Snapshot};

/// Copies from one starter before they count, unless Settings says.
pub const DEFAULT_AT: u32 = 50;

/// Many processes by design: a browser keeps one per site and extension.
const BY_DESIGN: &[&str] = &[
    "chrome.exe",
    "msedge.exe",
    "msedgewebview2.exe",
    "firefox.exe",
    "brave.exe",
    "opera.exe",
    "vivaldi.exe",
    "chrome",
    "chromium",
    "chromium-browser",
    "firefox",
    "firefox-bin",
    "Web Content",
    "Isolated Web Co",
    "WebExtensions",
    "brave",
    "msedge",
    "opera",
    "vivaldi-bin",
];

#[derive(Clone, Debug)]
pub struct Runaway {
    /// The program's name in lowercase: what "Don't warn" remembers.
    pub id: Arc<str>,
    pub name: Arc<str>,
    /// Its live copies from this starter, oldest first.
    pub copies: Vec<ProcKey>,
    /// Who started them; None once it has exited.
    pub starter: Option<(ProcKey, Arc<str>)>,
    /// Memory the copies use between them (private working set).
    pub memory: u64,
    /// Their CPU, of the whole machine's.
    pub cpu: f32,
}

impl Runaway {
    /// The same runaway a moment later: the program and who started it.
    pub fn same(&self, other: &Runaway) -> bool {
        self.id == other.id
            && self.starter.as_ref().map(|s| s.0) == other.starter.as_ref().map(|s| s.0)
    }

    /// "312 copies of node.exe, started by pwsh.exe (PID 1234)".
    pub fn headline(&self) -> String {
        format!(
            "{} copies of {}{}",
            self.copies.len(),
            self.name,
            match &self.starter {
                Some((key, name)) => format!(", started by {name} (PID {})", key.pid),
                None => ", whose starter has exited".into(),
            }
        )
    }
}

/// The runaways in `snap`: programs with at least `at` live copies from one
/// starter, the biggest first. `ignored` holds lowercase names.
pub fn find(snap: &Snapshot, at: u32, ignored: &[String]) -> Vec<Runaway> {
    let procs = &snap.processes;
    let parents = crate::tree::parents(procs);
    // (program, starter's index or, once it is gone, its PID) -> copies.
    let mut groups: HashMap<(String, Result<u32, u32>), Vec<u32>> = HashMap::new();
    for (i, p) in procs.iter().enumerate() {
        if p.state == ProcState::Exited || p.is_pseudo() || crate::platform::system_pid(p.pid) {
            continue;
        }
        let id = p.name.to_lowercase();
        if BY_DESIGN.iter().any(|b| b.eq_ignore_ascii_case(&p.name)) || ignored.contains(&id) {
            continue;
        }
        let mut top = i;
        for _ in 0..256 {
            match parents[top] {
                Some(up) if procs[up as usize].name.eq_ignore_ascii_case(&p.name) => {
                    top = up as usize
                }
                _ => break,
            }
        }
        let starter = match parents[top] {
            Some(s) => {
                let starter = &procs[s as usize];
                // The system's own: services, and what the kernel runs.
                if crate::platform::system_pid(starter.pid)
                    || starter.name.eq_ignore_ascii_case("services.exe")
                {
                    continue;
                }
                Ok(s)
            }
            None => Err(procs[top].ppid),
        };
        groups.entry((id, starter)).or_default().push(i as u32);
    }
    let mut runaways: Vec<Runaway> = groups
        .into_iter()
        .filter(|(_, copies)| copies.len() >= at.max(2) as usize)
        .map(|((id, starter), mut copies)| {
            copies.sort_by_key(|&c| procs[c as usize].key.create_time);
            let first = &procs[copies[0] as usize];
            Runaway {
                id: id.into(),
                name: first.name.clone(),
                memory: copies
                    .iter()
                    .map(|&c| procs[c as usize].private_working_set)
                    .sum(),
                cpu: copies.iter().map(|&c| procs[c as usize].cpu).sum(),
                copies: copies.iter().map(|&c| procs[c as usize].key).collect(),
                starter: starter.ok().map(|s| {
                    let s = &procs[s as usize];
                    (s.key, s.name.clone())
                }),
            }
        })
        .collect();
    runaways.sort_by(|a, b| b.copies.len().cmp(&a.copies.len()).then(a.id.cmp(&b.id)));
    runaways
}

#[cfg(test)]
mod tests {
    use super::*;
    use anabolic_core::ProcessInfo;
    use anabolic_core::history::History;

    fn p(pid: u32, ppid: u32, created: i64, name: &str) -> ProcessInfo {
        ProcessInfo::blank(
            ProcKey {
                pid,
                create_time: created,
            },
            ppid,
            Arc::from(name),
        )
    }

    fn snapshot(processes: Vec<ProcessInfo>) -> Snapshot {
        let mut snap = Snapshot::empty(1, Arc::new(History::new(4, 1, &[])));
        snap.processes = processes;
        snap.processes.sort_by_key(|p| p.pid);
        snap
    }

    #[test]
    fn counts_copies_per_starter() {
        let mut procs = vec![p(100, 1, 1, "pwsh.exe"), p(200, 1, 1, "bash.exe")];
        // Sixty from pwsh, some of them started by other copies.
        for i in 0..60 {
            let parent = if i % 2 == 0 { 100 } else { 1000 + (i - 1) * 4 };
            procs.push(p(1000 + i * 4, parent, 10 + i as i64, "node.exe"));
        }
        // Ten from bash: below the line.
        for i in 0..10 {
            procs.push(p(5000 + i * 4, 200, 10, "node.exe"));
        }
        let runaways = find(&snapshot(procs), 50, &[]);
        assert_eq!(runaways.len(), 1);
        let s = &runaways[0];
        assert_eq!(s.copies.len(), 60);
        assert_eq!(s.starter.as_ref().map(|s| s.0.pid), Some(100));
        assert_eq!(s.copies[0].pid, 1000, "oldest first");
        assert!(
            s.headline()
                .starts_with("60 copies of node.exe, started by pwsh.exe")
        );
    }

    #[test]
    fn leaves_out_browsers_service_hosts_and_the_ignored() {
        let mut procs = vec![p(100, 1, 1, "explorer.exe"), p(700, 1, 1, "services.exe")];
        for i in 0..80 {
            procs.push(p(1000 + i * 4, 100, 10, "chrome.exe"));
            procs.push(p(2000 + i * 4, 700, 10, "svchost.exe"));
            procs.push(p(3000 + i * 4, 100, 10, "worker.exe"));
        }
        let snap = snapshot(procs);
        let ids: Vec<Arc<str>> = find(&snap, 50, &[]).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![Arc::from("worker.exe")]);
        assert!(find(&snap, 50, &["worker.exe".into()]).is_empty());
    }

    #[test]
    fn copies_whose_starter_exited_still_count() {
        let mut procs = Vec::new();
        for i in 0..55 {
            procs.push(p(1000 + i * 4, 900, 10, "cmd.exe"));
        }
        let runaways = find(&snapshot(procs), 50, &[]);
        assert_eq!(runaways.len(), 1);
        assert!(runaways[0].starter.is_none());
        assert!(runaways[0].headline().ends_with("whose starter has exited"));
    }
}
