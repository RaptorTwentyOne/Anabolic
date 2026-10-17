//! Who started each process, kept for the whole session: a parent that has
//! exited is still named, with when it ran and how it ended. Fed by the
//! kernel's process start and stop events (so a launcher that lives a few
//! milliseconds is not missed) and by every process scan (for what was
//! running before Anabolic started).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::model::{ProcKey, ProcState, ProcessInfo};
use crate::nettrace::{ProcessStart, ProcessStop};
use crate::winfile::DeviceMap;

/// A process another one descends from, as seen this session.
#[derive(Clone, Debug)]
pub struct Ancestor {
    pub key: ProcKey,
    pub name: Arc<str>,
    /// Its file (drive-letter form), from its start event; None when only a
    /// scan saw it.
    pub image: Option<Arc<str>>,
    pub ppid: u32,
    /// When it exited (FILETIME) and its exit code when the stop event
    /// said; None while it runs.
    pub exited: Option<(i64, Option<u32>)>,
    /// Ran elevated (administrator), from its start event.
    pub elevated: Option<bool>,
}

struct Record {
    ancestor: Ancestor,
    parent: Option<ProcKey>,
}

/// Records kept; past this, the longest-exited go first.
const KEEP: usize = 100_000;

pub struct Lineage {
    records: HashMap<ProcKey, Record>,
    by_pid: HashMap<u32, Vec<ProcKey>>,
    devices: DeviceMap,
}

impl Default for Lineage {
    fn default() -> Self {
        Self {
            records: HashMap::new(),
            by_pid: HashMap::new(),
            devices: DeviceMap::build(),
        }
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

impl Lineage {
    /// The process `ppid` was when a child was created at `created`: the
    /// last one with that PID started before it. PIDs are reused.
    fn parent_of(&self, ppid: u32, created: i64) -> Option<ProcKey> {
        self.by_pid
            .get(&ppid)?
            .iter()
            .filter(|k| k.create_time <= created)
            .max_by_key(|k| k.create_time)
            .copied()
    }

    fn insert(&mut self, ancestor: Ancestor) {
        let key = ancestor.key;
        if self.records.contains_key(&key) {
            return;
        }
        let parent = self.parent_of(ancestor.ppid, key.create_time);
        self.by_pid.entry(key.pid).or_default().push(key);
        self.records.insert(key, Record { ancestor, parent });
    }

    /// Once per refresh: the starts and stops seen since the last one, and
    /// the processes running now.
    pub fn observe(
        &mut self,
        processes: &[ProcessInfo],
        starts: Vec<ProcessStart>,
        stops: Vec<ProcessStop>,
        now: i64,
    ) {
        for s in starts {
            let image = self.devices.to_dos_path(&s.image).unwrap_or(s.image);
            let name: Arc<str> = file_name(&image).into();
            self.insert(Ancestor {
                key: ProcKey {
                    pid: s.pid,
                    create_time: s.create_time,
                },
                name,
                image: Some(image.into()),
                ppid: s.ppid,
                exited: None,
                elevated: Some(s.elevated),
            });
        }
        let mut live = HashSet::with_capacity(processes.len());
        for p in processes.iter().filter(|p| !p.is_pseudo()) {
            if p.state != ProcState::Exited {
                live.insert(p.key);
            }
            self.insert(Ancestor {
                key: p.key,
                name: p.name.clone(),
                image: None,
                ppid: p.ppid,
                exited: None,
                elevated: None,
            });
            // Its start event can arrive before a scan sees it: it was not
            // gone after all.
            if p.state != ProcState::Exited
                && let Some(r) = self.records.get_mut(&p.key)
                && matches!(r.ancestor.exited, Some((_, None)))
            {
                r.ancestor.exited = None;
            }
        }
        for s in stops {
            let key = ProcKey {
                pid: s.pid,
                create_time: s.create_time,
            };
            if let Some(r) = self.records.get_mut(&key) {
                r.ancestor.exited = Some((s.exit_time, Some(s.exit_code)));
            }
        }
        // Gone without a stop event (no trace without administrator rights).
        for r in self.records.values_mut() {
            if r.ancestor.exited.is_none() && !live.contains(&r.ancestor.key) {
                r.ancestor.exited = Some((now, None));
            }
        }
        if self.records.len() > KEEP {
            let mut gone: Vec<(i64, ProcKey)> = self
                .records
                .values()
                .filter_map(|r| r.ancestor.exited.map(|(at, _)| (at, r.ancestor.key)))
                .collect();
            gone.sort_unstable();
            for (_, key) in gone.into_iter().take(KEEP / 5) {
                self.records.remove(&key);
                if let Some(keys) = self.by_pid.get_mut(&key.pid) {
                    keys.retain(|k| *k != key);
                }
            }
        }
    }

    /// The process' parent, its parent, and so on, as far as is known.
    /// The last one's `ppid` is the first unknown (exited before Anabolic
    /// started), unless it is 0.
    pub fn ancestry(&self, key: ProcKey) -> Vec<Ancestor> {
        // A record may have come before its parent's (one scan lists them
        // by PID, and a child can have the lower one): look again now.
        let parent = |r: &Record| {
            r.parent
                .or_else(|| self.parent_of(r.ancestor.ppid, r.ancestor.key.create_time))
                .filter(|p| *p != r.ancestor.key)
        };
        let mut chain = Vec::new();
        let mut at = self.records.get(&key).and_then(parent);
        while let Some(k) = at {
            let Some(r) = self.records.get(&k) else {
                break;
            };
            // A loop would mean a PID mix-up; stop rather than spin.
            if chain.len() >= 32 || chain.iter().any(|a: &Ancestor| a.key == k) {
                break;
            }
            chain.push(r.ancestor.clone());
            at = parent(r);
        }
        chain
    }

    /// The process' own record, when its start event was seen (its image
    /// path and elevation).
    pub fn get(&self, key: ProcKey) -> Option<&Ancestor> {
        self.records.get(&key).map(|r| &r.ancestor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(pid: u32, ppid: u32, at: i64, image: &str) -> ProcessStart {
        ProcessStart {
            pid,
            create_time: at,
            ppid,
            elevated: false,
            image: image.into(),
        }
    }

    #[test]
    fn names_a_parent_that_exited() {
        let mut l = Lineage::default();
        l.observe(
            &[],
            vec![
                start(10, 4, 100, r"C:\Windows\explorer.exe"),
                start(20, 10, 200, r"C:\Setup\setup.exe"),
                start(30, 20, 300, r"C:\App\app.exe"),
            ],
            vec![ProcessStop {
                pid: 20,
                create_time: 200,
                exit_time: 350,
                exit_code: 0,
            }],
            400,
        );
        let chain = l.ancestry(ProcKey {
            pid: 30,
            create_time: 300,
        });
        let names: Vec<&str> = chain.iter().map(|a| &*a.name).collect();
        assert_eq!(names, ["setup.exe", "explorer.exe"]);
        assert_eq!(chain[0].exited, Some((350, Some(0))));
    }

    #[test]
    fn a_reused_pid_is_not_the_parent() {
        let mut l = Lineage::default();
        // PID 20 ran, exited, and was reused after the child started.
        l.observe(
            &[],
            vec![
                start(20, 4, 100, "old.exe"),
                start(30, 20, 200, "child.exe"),
                start(20, 4, 300, "new.exe"),
            ],
            vec![],
            400,
        );
        let chain = l.ancestry(ProcKey {
            pid: 30,
            create_time: 200,
        });
        assert_eq!(&*chain[0].name, "old.exe");
    }

    #[test]
    fn a_child_scanned_before_its_parent_still_finds_it() {
        use crate::model::ProcessInfo;
        let child = ProcessInfo::blank(
            ProcKey {
                pid: 5,
                create_time: 200,
            },
            90,
            "ping.exe".into(),
        );
        let parent = ProcessInfo::blank(
            ProcKey {
                pid: 90,
                create_time: 100,
            },
            4,
            "cmd.exe".into(),
        );
        let mut l = Lineage::default();
        // Sorted by PID, as a scan lists them: the child first.
        l.observe(&[child, parent], vec![], vec![], 300);
        let chain = l.ancestry(ProcKey {
            pid: 5,
            create_time: 200,
        });
        assert_eq!(&*chain[0].name, "cmd.exe");
    }

    /// The real trace: a launcher that exits at once is still the parent.
    /// Needs an elevated test run (the trace does); skips itself otherwise.
    #[test]
    fn a_launcher_that_exited_is_named() {
        use std::os::windows::process::CommandExt;
        if !crate::hardening::is_elevated() {
            return;
        }
        let _one_at_a_time = crate::nettrace::TEST_SESSION.lock();
        let tracer = crate::nettrace::NetTracer::start().expect("trace");
        std::thread::sleep(std::time::Duration::from_millis(500));
        // cmd starts ping and exits; ping lives on for a couple of seconds.
        let mut launcher = std::process::Command::new("cmd.exe")
            .args(["/c", "start", "/b", "ping", "-n", "3", "127.0.0.1"])
            .creation_flags(0x0800_0000)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let cmd_pid = launcher.id();
        std::thread::sleep(std::time::Duration::from_millis(2500));
        let _ = launcher.wait();
        let traffic = tracer.take();
        let ping = traffic
            .starts
            .iter()
            .find(|s| s.ppid == cmd_pid && s.image.to_lowercase().ends_with("ping.exe"))
            .expect("ping's start event")
            .clone();
        let mut l = Lineage::default();
        l.observe(
            &[],
            traffic.starts,
            traffic.stops,
            crate::sysinfo::now_filetime(),
        );
        let chain = l.ancestry(ProcKey {
            pid: ping.pid,
            create_time: ping.create_time,
        });
        assert_eq!(&*chain[0].name, "cmd.exe", "{chain:?}");
        assert!(chain[0].exited.is_some(), "cmd has exited: {chain:?}");
        assert!(
            chain[0].image.as_deref().is_some_and(|i| i.contains(':')),
            "{chain:?}"
        );
    }
}
