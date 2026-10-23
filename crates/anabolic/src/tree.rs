// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Turns a snapshot's flat process list into the rows the table shows:
//! parent/child tree, flat list or per-executable groups, sorted, filtered
//! and collapsed.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anabolic_core::{
    DiskCounters, INTERRUPTS_PID, ProcKey, ProcState, ProcessDetails, ProcessInfo,
};

use crate::settings::{Col, View};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    /// Index into `Snapshot::processes`; for a group row, its first member.
    pub idx: u32,
    pub depth: u16,
    pub has_children: bool,
    pub expanded: bool,
    /// Shown only as context for a filter match further down.
    pub dim: bool,
    /// Set on group header rows (Apps view): index into `Built::groups`.
    pub group: Option<u32>,
    /// The last of its siblings shown: its guide line ends at this row.
    pub last: bool,
    /// Bit n: the guide line of depth n runs on past this row, to a later
    /// sibling of this row's ancestor at that depth.
    pub rails: u64,
}

/// Several processes of the same executable, with summed counters.
#[derive(Clone, Debug)]
pub struct Group {
    /// Lowercase executable name.
    pub id: Arc<str>,
    /// Totals over the live members; key, name and owner come from the first.
    pub agg: ProcessInfo,
    pub members: Vec<u32>,
}

#[derive(Default)]
pub struct Built {
    pub rows: Vec<Row>,
    pub groups: Vec<Group>,
}

pub type Details = HashMap<ProcKey, Arc<ProcessDetails>>;

pub struct BuildInput<'a> {
    pub procs: &'a [ProcessInfo],
    pub details: &'a Details,
    pub sort: Col,
    pub descending: bool,
    pub view: View,
    pub collapsed: &'a HashSet<ProcKey>,
    pub expanded_groups: &'a HashSet<Arc<str>>,
    /// Lowercase; empty for no filter.
    pub filter: &'a str,
}

const MAX_DEPTH: u16 = 256;

fn row(idx: u32, depth: u16) -> Row {
    Row {
        idx,
        depth,
        has_children: false,
        expanded: false,
        dim: false,
        group: None,
        last: false,
        rails: 0,
    }
}

pub fn build(input: &BuildInput<'_>) -> Built {
    let procs = input.procs;
    let filtering = !input.filter.is_empty();
    let matched: Vec<bool> = if filtering {
        procs
            .iter()
            .map(|p| matches(p, input.details.get(&p.key), input.filter))
            .collect()
    } else {
        Vec::new()
    };
    let shown = |i: u32| !filtering || matched[i as usize];
    let cmp = |&a: &u32, &b: &u32| compare(&procs[a as usize], &procs[b as usize], input);

    match input.view {
        View::List => {
            let mut idx: Vec<u32> = (0..procs.len() as u32).filter(|&i| shown(i)).collect();
            idx.sort_by(cmp);
            Built {
                rows: idx.into_iter().map(|i| row(i, 0)).collect(),
                groups: Vec::new(),
            }
        }
        View::Apps => build_apps(input, &shown, filtering),
        View::Tree => build_tree(input, &matched, filtering),
    }
}

fn build_tree(input: &BuildInput<'_>, matched: &[bool], filtering: bool) -> Built {
    let procs = input.procs;
    let cmp = |&a: &u32, &b: &u32| compare(&procs[a as usize], &procs[b as usize], input);
    let parents = parents(procs);
    let mut children: Vec<Vec<u32>> = vec![Vec::new(); procs.len()];
    let mut roots = Vec::new();
    for (i, parent) in parents.iter().enumerate() {
        match parent {
            Some(p) => children[*p as usize].push(i as u32),
            None => roots.push(i as u32),
        }
    }
    roots.sort_by(cmp);
    for list in &mut children {
        list.sort_by(cmp);
    }

    let visible = if filtering {
        let mut visible = vec![false; procs.len()];
        for &r in &roots {
            mark_visible(r, &children, matched, &mut visible, 0);
        }
        visible
    } else {
        Vec::new()
    };

    let mut rows = Vec::with_capacity(procs.len());
    let walk = Walk {
        procs,
        children: &children,
        visible: &visible,
        matched,
        collapsed: input.collapsed,
        filtering,
    };
    for &r in &roots {
        walk.walk(r, 0, 0, true, &mut rows);
    }
    Built {
        rows,
        groups: Vec::new(),
    }
}

fn build_apps(input: &BuildInput<'_>, shown: &dyn Fn(u32) -> bool, filtering: bool) -> Built {
    let procs = input.procs;
    let cmp = |&a: &u32, &b: &u32| compare(&procs[a as usize], &procs[b as usize], input);
    let mut by_name: HashMap<String, Vec<u32>> = HashMap::new();
    for (i, p) in procs.iter().enumerate() {
        if shown(i as u32) {
            by_name
                .entry(p.name.to_ascii_lowercase())
                .or_default()
                .push(i as u32);
        }
    }

    enum Item {
        Group(usize),
        Single(u32),
    }
    let mut groups = Vec::new();
    let mut items = Vec::with_capacity(by_name.len());
    for (id, mut members) in by_name {
        if members.len() == 1 {
            items.push(Item::Single(members[0]));
            continue;
        }
        let agg = aggregate(procs, &members);
        members.sort_by(cmp);
        groups.push(Group {
            id: Arc::from(id),
            agg,
            members,
        });
        items.push(Item::Group(groups.len() - 1));
    }
    let info = |item: &Item| match item {
        Item::Group(g) => &groups[*g].agg,
        Item::Single(i) => &procs[*i as usize],
    };
    items.sort_by(|a, b| compare(info(a), info(b), input));

    let mut rows = Vec::with_capacity(procs.len());
    for item in &items {
        match *item {
            Item::Single(i) => rows.push(row(i, 0)),
            Item::Group(g) => {
                let group = &groups[g];
                let expanded = filtering || input.expanded_groups.contains(&group.id);
                rows.push(Row {
                    idx: group.members[0],
                    depth: 0,
                    has_children: true,
                    expanded,
                    dim: false,
                    group: Some(g as u32),
                    last: false,
                    rails: 0,
                });
                if expanded {
                    let n = group.members.len();
                    rows.extend(group.members.iter().enumerate().map(|(i, &m)| Row {
                        last: i + 1 == n,
                        ..row(m, 1)
                    }));
                }
            }
        }
    }
    Built { rows, groups }
}

/// Sums the live members' counters into one synthetic process.
pub fn aggregate(procs: &[ProcessInfo], members: &[u32]) -> ProcessInfo {
    let first = &procs[members[0] as usize];
    let mut a = ProcessInfo::blank(first.key, first.ppid, first.name.clone());
    a.session_id = first.session_id;
    a.base_priority = first.base_priority;
    a.user_sid = first.user_sid.clone();
    let (mut alive, mut all_suspended) = (false, true);
    let mut disk: Option<DiskCounters> = None;
    for &m in members {
        let p = &procs[m as usize];
        if p.state == ProcState::Exited {
            continue;
        }
        alive = true;
        all_suspended &= p.state == ProcState::Suspended;
        a.not_responding |= p.not_responding;
        a.cpu += p.cpu;
        a.cpu_kernel += p.cpu_kernel;
        a.cpu_time += p.cpu_time;
        a.cycles_delta += p.cycles_delta;
        a.private_bytes += p.private_bytes;
        a.working_set += p.working_set;
        a.private_working_set += p.private_working_set;
        a.paged_pool += p.paged_pool;
        a.nonpaged_pool += p.nonpaged_pool;
        a.page_faults_delta += p.page_faults_delta;
        a.io.read_bytes += p.io.read_bytes;
        a.io.write_bytes += p.io.write_bytes;
        a.io.other_bytes += p.io.other_bytes;
        a.io_read_rate += p.io_read_rate;
        a.io_write_rate += p.io_write_rate;
        a.io_other_rate += p.io_other_rate;
        if let Some(d) = p.disk {
            let t = disk.get_or_insert_default();
            t.read_bytes += d.read_bytes;
            t.write_bytes += d.write_bytes;
        }
        a.disk_read_rate += p.disk_read_rate;
        a.disk_write_rate += p.disk_write_rate;
        a.threads += p.threads;
        a.handles += p.handles;
        a.context_switches_delta += p.context_switches_delta;
        a.handles_per_min += p.handles_per_min;
        a.gpu += p.gpu;
        a.net_rx_rate += p.net_rx_rate;
        a.net_tx_rate += p.net_tx_rate;
        a.net_rx_total += p.net_rx_total;
        a.net_tx_total += p.net_tx_total;
    }
    a.disk = disk;
    a.state = match (alive, all_suspended) {
        (false, _) => ProcState::Exited,
        (true, true) => ProcState::Suspended,
        (true, false) => ProcState::Running,
    };
    a
}

/// Parent index for every process. A parent must have started before its
/// child, otherwise the PID was recycled and the real parent is gone.
pub fn parents(procs: &[ProcessInfo]) -> Vec<Option<u32>> {
    let mut by_pid: HashMap<u32, Vec<u32>> = HashMap::with_capacity(procs.len());
    for (i, p) in procs.iter().enumerate() {
        by_pid.entry(p.pid).or_default().push(i as u32);
    }
    procs
        .iter()
        .enumerate()
        .map(|(i, p)| {
            // The idle process and System head their own trees.
            if p.pid == 0 || p.pid == anabolic_core::scan::KERNEL_PID {
                return None;
            }
            by_pid
                .get(&p.ppid)?
                .iter()
                .copied()
                .filter(|&j| j as usize != i)
                .filter(|&j| {
                    let q = &procs[j as usize];
                    q.key.create_time <= p.key.create_time || p.pid == INTERRUPTS_PID
                })
                .max_by_key(|&j| procs[j as usize].key.create_time)
        })
        .collect()
}

fn mark_visible(
    i: u32,
    children: &[Vec<u32>],
    matched: &[bool],
    visible: &mut [bool],
    depth: u16,
) -> bool {
    let mut any = matched[i as usize];
    if depth < MAX_DEPTH {
        for &c in &children[i as usize] {
            any |= mark_visible(c, children, matched, visible, depth + 1);
        }
    }
    visible[i as usize] = any;
    any
}

struct Walk<'a> {
    procs: &'a [ProcessInfo],
    children: &'a [Vec<u32>],
    visible: &'a [bool],
    matched: &'a [bool],
    collapsed: &'a HashSet<ProcKey>,
    filtering: bool,
}

impl Walk<'_> {
    fn walk(&self, i: u32, depth: u16, rails: u64, last: bool, rows: &mut Vec<Row>) {
        let iu = i as usize;
        if self.filtering && !self.visible[iu] {
            return;
        }
        let kids = &self.children[iu];
        let has_children = kids
            .iter()
            .any(|&c| !self.filtering || self.visible[c as usize]);
        let expanded = self.filtering || !self.collapsed.contains(&self.procs[iu].key);
        rows.push(Row {
            idx: i,
            depth,
            has_children,
            expanded,
            dim: self.filtering && !self.matched[iu],
            group: None,
            last,
            rails,
        });
        if expanded && depth < MAX_DEPTH {
            let shown: Vec<u32> = kids
                .iter()
                .copied()
                .filter(|&c| !self.filtering || self.visible[c as usize])
                .collect();
            // Our own line continues past our children to our next sibling.
            let rails = if last || depth == 0 {
                rails
            } else {
                rails | 1u64.checked_shl(depth.into()).unwrap_or(0)
            };
            for (n, &c) in shown.iter().enumerate() {
                self.walk(c, depth + 1, rails, n + 1 == shown.len(), rows);
            }
        }
    }
}

fn contains_ci(hay: &str, needle_lower: &str) -> bool {
    if hay.is_ascii() && needle_lower.is_ascii() {
        let (h, n) = (hay.as_bytes(), needle_lower.as_bytes());
        n.len() <= h.len()
            && h.windows(n.len())
                .any(|w| w.iter().zip(n).all(|(a, b)| a.to_ascii_lowercase() == *b))
    } else {
        hay.to_lowercase().contains(needle_lower)
    }
}

fn matches(p: &ProcessInfo, d: Option<&Arc<ProcessDetails>>, filter: &str) -> bool {
    if contains_ci(&p.name, filter) || p.pid.to_string() == filter {
        return true;
    }
    let Some(d) = d else { return false };
    [
        &d.path,
        &d.description,
        &d.company,
        &d.command_line,
        &d.user,
    ]
    .into_iter()
    .any(|field| field.as_deref().is_some_and(|s| contains_ci(s, filter)))
}

fn text<'a>(details: &'a Details, p: &ProcessInfo, col: Col) -> Option<&'a str> {
    let d = details.get(&p.key)?;
    match col {
        // As shown: by account, not by domain first.
        Col::User => d.user.as_deref().map(crate::fmt::account),
        Col::Integrity => d.integrity.map(|i| i.label()),
        Col::Arch => d.arch.map(|a| a.label()),
        Col::Description => d.description.as_deref(),
        Col::Company => d.company.as_deref(),
        Col::Version => d.version.as_deref(),
        Col::Path => d.path.as_deref(),
        Col::CommandLine => d.command_line.as_deref(),
        _ => None,
    }
}

fn ci_cmp(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|c| c.to_ascii_lowercase())
        .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

pub fn compare(a: &ProcessInfo, b: &ProcessInfo, input: &BuildInput<'_>) -> Ordering {
    let f = |x: f64, y: f64| x.partial_cmp(&y).unwrap_or(Ordering::Equal);
    let primary = match input.sort {
        Col::Name => ci_cmp(&a.name, &b.name),
        Col::Pid => a.pid.cmp(&b.pid),
        Col::Cpu => f(a.cpu as f64, b.cpu as f64),
        Col::CpuTime => a.cpu_time.cmp(&b.cpu_time),
        Col::Private => a.private_bytes.cmp(&b.private_bytes),
        Col::WorkingSet => a.working_set.cmp(&b.working_set),
        Col::PrivateWs => a.private_working_set.cmp(&b.private_working_set),
        Col::Disk => f(a.disk_total_rate(), b.disk_total_rate()),
        Col::Io => f(a.io_total_rate(), b.io_total_rate()),
        Col::Gpu => f(a.gpu as f64, b.gpu as f64),
        Col::Net => f(a.net_total_rate(), b.net_total_rate()),
        Col::Threads => a.threads.cmp(&b.threads),
        Col::Handles => a.handles.cmp(&b.handles),
        Col::HandleTrend => f(a.handles_per_min as f64, b.handles_per_min as f64),
        Col::Priority => a.base_priority.cmp(&b.base_priority),
        Col::Session => a.session_id.cmp(&b.session_id),
        col => match (text(input.details, a, col), text(input.details, b, col)) {
            (Some(x), Some(y)) => ci_cmp(x, y),
            // Unknown values sink to the bottom in both directions.
            (Some(_), None) => return Ordering::Less,
            (None, Some(_)) => return Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
    };
    let primary = if input.descending {
        primary.reverse()
    } else {
        primary
    };
    primary
        .then(a.pid.cmp(&b.pid))
        .then(a.key.create_time.cmp(&b.key.create_time))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, created: i64, name: &str, cpu: f32) -> ProcessInfo {
        let mut p = ProcessInfo::blank(
            ProcKey {
                pid,
                create_time: created,
            },
            ppid,
            Arc::from(name),
        );
        p.cpu = cpu;
        p
    }

    struct Fixture {
        details: Details,
        collapsed: HashSet<ProcKey>,
        expanded: HashSet<Arc<str>>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                details: Details::new(),
                collapsed: HashSet::new(),
                expanded: HashSet::new(),
            }
        }

        fn input<'a>(
            &'a self,
            procs: &'a [ProcessInfo],
            view: View,
            filter: &'a str,
        ) -> BuildInput<'a> {
            BuildInput {
                procs,
                details: &self.details,
                sort: Col::Cpu,
                descending: true,
                view,
                collapsed: &self.collapsed,
                expanded_groups: &self.expanded,
                filter,
            }
        }
    }

    fn names(procs: &[ProcessInfo], built: &Built) -> Vec<String> {
        built
            .rows
            .iter()
            .map(|r| match r.group {
                Some(g) => format!(
                    "[{} x{}]",
                    built.groups[g as usize].id,
                    built.groups[g as usize].members.len()
                ),
                None => format!(
                    "{}{}",
                    " ".repeat(r.depth as usize),
                    procs[r.idx as usize].name
                ),
            })
            .collect()
    }

    #[test]
    fn builds_sorted_tree() {
        // The kernel's process (System, PID 4 on Windows; PID 2 on Linux).
        let kernel = anabolic_core::scan::KERNEL_PID;
        let procs = vec![
            p(0, 0, 0, "Idle", 90.0),
            p(kernel, 0, 0, "System", 1.0),
            p(100, kernel, 10, "smss", 0.0),
            p(200, 50, 20, "explorer", 2.0), // parent 50 never existed
            p(300, 200, 30, "chrome", 5.0),
            p(301, 200, 31, "notepad", 0.5),
            p(INTERRUPTS_PID, kernel, 0, "Interrupts", 0.1),
        ];
        let f = Fixture::new();
        let built = build(&f.input(&procs, View::Tree, ""));
        assert_eq!(
            names(&procs, &built),
            [
                "Idle",
                "explorer",
                " chrome",
                " notepad",
                "System",
                " Interrupts",
                " smss"
            ]
        );
    }

    #[test]
    fn recycled_pid_is_not_a_parent() {
        // 500 was reused by a process started after its supposed child.
        let procs = vec![
            p(500, 1, 100, "newcomer", 0.0),
            p(600, 500, 50, "orphan", 0.0),
        ];
        assert_eq!(parents(&procs), vec![None, None]);
    }

    #[test]
    fn collapse_hides_children() {
        let procs = vec![p(10, 1, 1, "parent", 0.0), p(11, 10, 2, "child", 0.0)];
        let mut f = Fixture::new();
        f.collapsed.insert(procs[0].key);
        let built = build(&f.input(&procs, View::Tree, ""));
        assert_eq!(built.rows.len(), 1);
        assert!(built.rows[0].has_children && !built.rows[0].expanded);
    }

    #[test]
    fn filter_keeps_ancestors_dimmed() {
        let procs = vec![
            p(10, 1, 1, "explorer", 0.0),
            p(11, 10, 2, "code", 0.0),
            p(12, 10, 3, "notepad", 0.0),
        ];
        let f = Fixture::new();
        let built = build(&f.input(&procs, View::Tree, "note"));
        assert_eq!(names(&procs, &built), ["explorer", " notepad"]);
        assert!(built.rows[0].dim && !built.rows[1].dim);
    }

    #[test]
    fn flat_list_sorts_globally() {
        let procs = vec![
            p(10, 1, 1, "a", 1.0),
            p(11, 10, 2, "b", 3.0),
            p(12, 10, 3, "c", 2.0),
        ];
        let f = Fixture::new();
        assert_eq!(
            names(&procs, &build(&f.input(&procs, View::List, ""))),
            ["b", "c", "a"]
        );
    }

    #[test]
    fn apps_group_by_executable_and_sum() {
        let mut procs = vec![
            p(10, 1, 1, "chrome.exe", 1.0),
            p(11, 10, 2, "chrome.exe", 2.0),
            p(12, 10, 3, "Chrome.exe", 0.5),
            p(20, 1, 4, "game.exe", 3.0),
        ];
        procs[1].private_working_set = 20 << 30;
        procs[2].private_working_set = 2 << 30;
        let mut f = Fixture::new();
        let built = build(&f.input(&procs, View::Apps, ""));
        // chrome's 3.5% total beats the single game process.
        assert_eq!(names(&procs, &built), ["[chrome.exe x3]", "game.exe"]);
        let chrome = &built.groups[0];
        assert_eq!(chrome.agg.private_working_set, 22 << 30);
        assert!((chrome.agg.cpu - 3.5).abs() < 1e-6);

        f.expanded.insert(chrome.id.clone());
        let built = build(&f.input(&procs, View::Apps, ""));
        assert_eq!(
            names(&procs, &built),
            [
                "[chrome.exe x3]",
                " chrome.exe",
                " chrome.exe",
                " Chrome.exe",
                "game.exe"
            ]
        );
    }
}
