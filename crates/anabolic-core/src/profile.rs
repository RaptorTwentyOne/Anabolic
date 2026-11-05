// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Where a process spends its processor time: for a few seconds the system
//! samples what each processor runs, with the call stack, a thousand times
//! a second; the samples of one process are summed by function (its own
//! time and with what it calls), by module and by thread, and as a call
//! tree. On Windows the samples come from the kernel's own profiler (ETW),
//! so the process is never opened; names come from the modules' files, with
//! symbols where they are on this machine and exports where not.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

#[cfg_attr(windows, path = "windows/profile.rs")]
#[cfg_attr(target_os = "linux", path = "linux/profile.rs")]
mod imp;
pub use imp::profile;

/// A place in code: a function of a module, or a module whose code has no
/// names here.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Frame {
    /// The module's file name ("ntdll.dll"); "?" outside every module.
    pub module: Arc<str>,
    /// Empty when the module's code has no names here.
    pub function: Arc<str>,
    /// In the system's kernel or a driver.
    pub kernel: bool,
}

impl Frame {
    pub fn label(&self) -> String {
        if self.function.is_empty() {
            format!("{} (no symbols)", self.module)
        } else {
            format!("{}!{}", self.module, self.function)
        }
    }
}

/// One sample: a thread and its stack, innermost frame first.
#[derive(Clone, Debug, Default)]
pub struct Sample {
    pub tid: u32,
    pub frames: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Hot {
    pub frame: u32,
    /// Samples it was running itself.
    pub own: u64,
    /// Samples it was on the stack, itself or what it called.
    pub total: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ModuleHot {
    pub module: Arc<str>,
    pub own: u64,
    pub total: u64,
}

/// A node of the call tree: a frame called from its parent's.
#[derive(Clone, Debug, Default)]
pub struct Node {
    /// `u32::MAX` for the root.
    pub frame: u32,
    pub total: u64,
    pub own: u64,
    /// Largest first.
    pub children: Vec<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub pid: u32,
    /// How long it sampled.
    pub took: Duration,
    pub samples: u64,
    pub frames: Vec<Frame>,
    /// Largest own time first.
    pub functions: Vec<Hot>,
    pub modules: Vec<ModuleHot>,
    /// (thread id, samples), the busiest first.
    pub threads: Vec<(u32, u64)>,
    /// Node 0 is the root.
    pub tree: Vec<Node>,
    /// What to know about the result, in words.
    pub notes: Vec<String>,
}

/// The call tree stops growing here; deeper calls count at their parent.
const MAX_NODES: usize = 400_000;

impl Profile {
    /// Sums the samples.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn build(
        pid: u32,
        took: Duration,
        frames: Vec<Frame>,
        samples: &[Sample],
        notes: Vec<String>,
    ) -> Profile {
        let mut own = vec![0u64; frames.len()];
        let mut total = vec![0u64; frames.len()];
        let mut threads: HashMap<u32, u64> = HashMap::new();
        let mut tree = vec![Node {
            frame: u32::MAX,
            ..Node::default()
        }];
        let mut child: HashMap<(u32, u32), u32> = HashMap::new();
        let mut seen = HashSet::new();
        for s in samples {
            *threads.entry(s.tid).or_default() += 1;
            let Some(&top) = s.frames.first() else {
                continue;
            };
            own[top as usize] += 1;
            seen.clear();
            for &f in &s.frames {
                if seen.insert(f) {
                    total[f as usize] += 1;
                }
            }
            // Down the tree from the outermost call.
            let mut at = 0u32;
            tree[0].total += 1;
            for &f in s.frames.iter().rev() {
                let next = match child.get(&(at, f)) {
                    Some(&n) => n,
                    None if tree.len() < MAX_NODES => {
                        let n = tree.len() as u32;
                        tree.push(Node {
                            frame: f,
                            ..Node::default()
                        });
                        tree[at as usize].children.push(n);
                        child.insert((at, f), n);
                        n
                    }
                    None => break,
                };
                at = next;
                tree[at as usize].total += 1;
            }
            tree[at as usize].own += 1;
        }
        // Children largest first.
        let totals: Vec<u64> = tree.iter().map(|n| n.total).collect();
        for n in &mut tree {
            n.children
                .sort_by_key(|&c| std::cmp::Reverse(totals[c as usize]));
        }
        let mut functions: Vec<Hot> = (0..frames.len())
            .filter(|&i| total[i] > 0)
            .map(|i| Hot {
                frame: i as u32,
                own: own[i],
                total: total[i],
            })
            .collect();
        functions.sort_by(|a, b| b.own.cmp(&a.own).then(b.total.cmp(&a.total)));
        // Modules: own from the innermost frame, total once per sample.
        let mut by_module: HashMap<Arc<str>, (u64, u64)> = HashMap::new();
        let mut modules_seen = HashSet::new();
        for s in samples {
            if let Some(&top) = s.frames.first() {
                by_module
                    .entry(frames[top as usize].module.clone())
                    .or_default()
                    .0 += 1;
            }
            modules_seen.clear();
            for &f in &s.frames {
                let m = &frames[f as usize].module;
                if modules_seen.insert(m.clone()) {
                    by_module.entry(m.clone()).or_default().1 += 1;
                }
            }
        }
        let mut modules: Vec<ModuleHot> = by_module
            .into_iter()
            .map(|(module, (own, total))| ModuleHot { module, own, total })
            .collect();
        modules.sort_by(|a, b| b.own.cmp(&a.own).then(b.total.cmp(&a.total)));
        let mut threads: Vec<(u32, u64)> = threads.into_iter().collect();
        threads.sort_by_key(|&(tid, n)| (std::cmp::Reverse(n), tid));
        Profile {
            pid,
            took,
            samples: samples.len() as u64,
            frames,
            functions,
            modules,
            threads,
            tree,
            notes,
        }
    }

    /// A share of the samples, %.
    pub fn share(&self, n: u64) -> f32 {
        if self.samples == 0 {
            0.0
        } else {
            n as f32 * 100.0 / self.samples as f32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(module: &str, function: &str) -> Frame {
        Frame {
            module: module.into(),
            function: function.into(),
            kernel: false,
        }
    }

    #[test]
    fn sums_samples_by_function_module_thread_and_call() {
        // main -> work -> hash (twice), main -> work (once), main -> wait
        // in the kernel (once); work calls itself once.
        let frames = vec![
            frame("app.exe", "main"),
            frame("app.exe", "work"),
            frame("hash.dll", "hash"),
            Frame {
                kernel: true,
                ..frame("ntoskrnl.exe", "KeWait")
            },
        ];
        let samples = [
            Sample {
                tid: 1,
                frames: vec![2, 1, 0],
            },
            Sample {
                tid: 1,
                frames: vec![2, 1, 1, 0],
            },
            Sample {
                tid: 2,
                frames: vec![1, 0],
            },
            Sample {
                tid: 2,
                frames: vec![3, 0],
            },
        ];
        let p = Profile::build(9, Duration::from_secs(1), frames, &samples, Vec::new());
        assert_eq!(p.samples, 4);
        assert_eq!(p.functions[0].frame, 2, "hash runs most itself");
        assert_eq!(p.functions[0].own, 2);
        let work = p.functions.iter().find(|h| h.frame == 1).unwrap();
        assert_eq!((work.own, work.total), (1, 3), "recursion counts once");
        let main = p.functions.iter().find(|h| h.frame == 0).unwrap();
        assert_eq!((main.own, main.total), (0, 4));
        assert_eq!(p.modules[0].module.as_ref(), "hash.dll");
        assert_eq!(
            p.modules
                .iter()
                .find(|m| &*m.module == "app.exe")
                .unwrap()
                .total,
            4
        );
        assert_eq!(p.threads, vec![(1, 2), (2, 2)]);
        // root -> main (4) -> work (3) -> hash (1) and work (1) -> hash (1)
        let root = &p.tree[0];
        assert_eq!(root.total, 4);
        let main = &p.tree[root.children[0] as usize];
        assert_eq!((main.frame, main.total), (0, 4));
        let work = &p.tree[main.children[0] as usize];
        assert_eq!((work.frame, work.total, work.own), (1, 3, 1));
        assert_eq!(work.children.len(), 2);
        assert_eq!(p.share(1), 25.0);
        assert_eq!(p.frames[3].label(), "ntoskrnl.exe!KeWait");
        assert_eq!(frame("x.dll", "").label(), "x.dll (no symbols)");
    }
}
