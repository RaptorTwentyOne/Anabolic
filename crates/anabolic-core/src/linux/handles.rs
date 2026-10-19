//! Open files as handles: what /proc/<pid>/fd lists, each named by the link
//! it is and typed by what it points at. Reading it needs the right to
//! look into the process (being its owner, or cap_sys_ptrace); nothing is
//! duplicated or opened, so every report comes with names.

use std::collections::HashMap;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::Arc;
use std::time::Instant;

use crate::model::ProcKey;
use crate::modules::Symbolizer;
use crate::procfs;

use super::*;

/// What an open file can be, by type index.
const TYPES: [&str; 15] = [
    "File",
    "Directory",
    "Socket",
    "Pipe",
    "Device",
    "GPU",
    "Shared memory",
    "Memory file",
    "Event",
    "Epoll",
    "Timer",
    "Signal",
    "File watch",
    "Process",
    "Other",
];

const FILE: u16 = 0;
const DIRECTORY: u16 = 1;
const SOCKET: u16 = 2;
const PIPE: u16 = 3;

/// Search results past this are dropped.
const MAX_MATCHES: usize = 5_000;

/// The type of an open file from where its link points (and, for a path,
/// whether that is a folder).
fn classify(target: &str, directory: bool) -> u16 {
    let index = |name: &str| TYPES.iter().position(|t| *t == name).unwrap_or(14) as u16;
    if target.starts_with("socket:[") {
        return SOCKET;
    }
    if target.starts_with("pipe:[") {
        return PIPE;
    }
    if let Some(kind) = target.strip_prefix("anon_inode:") {
        return index(match kind.trim_matches(['[', ']']) {
            "eventfd" => "Event",
            "eventpoll" => "Epoll",
            "timerfd" => "Timer",
            "signalfd" => "Signal",
            "inotify" | "fanotify" => "File watch",
            "pidfd" => "Process",
            _ => "Other",
        });
    }
    if target.starts_with("/memfd:") {
        return index("Memory file");
    }
    if target.starts_with("/dev/dri/") {
        return index("GPU");
    }
    if target.starts_with("/dev/shm/") {
        return index("Shared memory");
    }
    if target.starts_with("/dev/") {
        return index("Device");
    }
    if !target.starts_with('/') {
        return index("Other");
    }
    if directory { DIRECTORY } else { FILE }
}

/// One number for the file an open file points at: its device and inode.
fn object_id(meta: &std::fs::Metadata) -> u64 {
    (meta.dev() << 44) | (meta.ino() & ((1 << 44) - 1))
}

/// Why a process' open files could not be read, in words.
fn unreadable(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => {
            "another user's process: reading its open files needs cap_sys_ptrace".into()
        }
        std::io::ErrorKind::NotFound => "it has exited".into(),
        _ => crate::util::error_text(e),
    }
}

/// An open file as listed: number, link, the file it points at.
struct Open {
    fd: u64,
    target: String,
    meta: Option<std::fs::Metadata>,
    /// 1: open for reading, 2: for writing (the link's own permissions
    /// say which).
    access: u32,
}

fn open_files(pid: u32) -> Result<Vec<Open>, String> {
    let dir = std::fs::read_dir(format!("/proc/{pid}/fd")).map_err(|e| unreadable(&e))?;
    let mut out: Vec<Open> = dir
        .flatten()
        .filter_map(|e| {
            let fd = e.file_name().to_str()?.parse().ok()?;
            let path = e.path();
            let target = std::fs::read_link(&path)
                .ok()?
                .to_string_lossy()
                .into_owned();
            let mode = std::fs::symlink_metadata(&path).map_or(0, |m| m.permissions().mode());
            let access = u32::from(mode & 0o400 != 0) | u32::from(mode & 0o200 != 0) << 1;
            Some(Open {
                fd,
                meta: std::fs::metadata(&path).ok(),
                target,
                access,
            })
        })
        .collect();
    out.sort_by_key(|o| o.fd);
    Ok(out)
}

/// What to call an open file: its path, or the socket or pipe in words.
fn name_of(open: &Open, sockets: &HashMap<u64, String>) -> String {
    let inode = |prefix: &str| {
        open.target
            .strip_prefix(prefix)?
            .strip_suffix(']')?
            .parse::<u64>()
            .ok()
    };
    if let Some(name) = inode("socket:[").and_then(|i| sockets.get(&i)) {
        return name.clone();
    }
    if let Some(memfd) = open.target.strip_prefix("/memfd:") {
        return format!("memfd:{}", memfd.trim_end_matches(" (deleted)"));
    }
    open.target.clone()
}

/// Every object type, for their names.
pub fn object_types() -> HashMap<u16, ObjectTypeInfo> {
    TYPES
        .iter()
        .enumerate()
        .map(|(i, name)| {
            (
                i as u16,
                ObjectTypeInfo {
                    name: Arc::from(*name),
                    objects: 0,
                    handles: 0,
                },
            )
        })
        .collect()
}

/// Ended processes their parents have not collected (zombies), by parent.
pub fn zombie_report() -> Result<ZombieReport, String> {
    let started = Instant::now();
    let mut total = 0u32;
    let mut per_parent: HashMap<u32, usize> = HashMap::new();
    for pid in procfs::pids() {
        let Some(stat) = procfs::stat(pid) else {
            continue;
        };
        total += 1;
        if stat.state == b'Z' {
            *per_parent.entry(stat.ppid).or_default() += 1;
        }
    }
    let zombies: usize = per_parent.values().sum();
    let mut holders: Vec<ZombieHolder> = per_parent
        .into_iter()
        .map(|(pid, zombies)| ZombieHolder {
            pid,
            zombies,
            handles: 0,
        })
        .collect();
    holders.sort_by_key(|h| std::cmp::Reverse(h.zombies));
    Ok(ZombieReport {
        process_objects: total,
        running: total as usize - zombies,
        zombies,
        holders,
        took: started.elapsed(),
    })
}

/// Every process, zombies included.
pub fn process_objects() -> u32 {
    procfs::pids().len() as u32
}

/// Open files system-wide (the kernel's count of file objects in use).
fn system_total() -> usize {
    std::fs::read_to_string("/proc/sys/fs/file-nr")
        .ok()
        .and_then(|s| s.split_ascii_whitespace().next()?.parse().ok())
        .unwrap_or(0)
}

/// The open files of one process, with names (`names` is for Windows,
/// where naming costs a duplication; here it comes free).
pub fn handles_now(key: ProcKey, _names: bool) -> Result<HandleReport, String> {
    let started = Instant::now();
    if key.create_time != 0
        && procfs::stat(key.pid).is_none_or(|s| s.create_time() != key.create_time)
    {
        return Err("it has exited".into());
    }
    let open = open_files(key.pid)?;
    let sockets = if open.iter().any(|o| o.target.starts_with("socket:")) {
        crate::sockets::names_by_inode()
    } else {
        HashMap::new()
    };
    let entries: Vec<HandleEntry> = open
        .iter()
        .map(|o| HandleEntry {
            value: o.fd,
            type_index: classify(&o.target, o.meta.as_ref().is_some_and(|m| m.is_dir())),
            access: o.access,
            attributes: 0,
            object: o.meta.as_ref().map_or(0, object_id),
        })
        .collect();
    let names: Vec<Option<Arc<str>>> = open
        .iter()
        .map(|o| Some(Arc::from(name_of(o, &sockets))))
        .collect();
    let type_names = object_type_names();
    Ok(HandleReport {
        key,
        by_type: count_by_type(&entries, &type_names),
        type_names,
        resolved: names.len(),
        name_groups: name_groups(&entries, &names),
        names,
        name_error: None,
        system_total: system_total(),
        process_targets: None,
        entries,
        took: started.elapsed(),
    })
}

/// A process holding a file open: pid, name, its file numbers for it, and
/// their access together.
pub(crate) type Holding = (u32, Arc<str>, Vec<u64>, u32);

/// The processes with an open file pointing at `object` (see `object_id`),
/// those in `except` left out.
pub(crate) fn holders_of(object: u64, except: &[u32]) -> Result<Vec<Holding>, String> {
    if object == 0 {
        return Err("the file it points at is not known".into());
    }
    let mut out = Vec::new();
    for pid in procfs::pids() {
        if except.contains(&pid) {
            continue;
        }
        let Ok(open) = open_files(pid) else {
            continue;
        };
        let held: Vec<&Open> = open
            .iter()
            .filter(|o| o.meta.as_ref().is_some_and(|m| object_id(m) == object))
            .collect();
        if held.is_empty() {
            continue;
        }
        let name = procfs::stat(pid).map_or_else(|| "?".to_string(), |s| s.comm);
        out.push((
            pid,
            Arc::from(name),
            held.iter().map(|o| o.fd).collect(),
            held.iter().fold(0, |a, o| a | o.access),
        ));
    }
    Ok(out)
}

/// What the inspector thread keeps between requests: parsed symbol tables.
pub(super) struct Bench {
    symbols: Symbolizer,
}

impl Bench {
    pub(super) fn new() -> Self {
        Self {
            symbols: Symbolizer::default(),
        }
    }

    pub(super) fn handles(&mut self, key: ProcKey, names: bool) -> Result<HandleReport, String> {
        handles_now(key, names)
    }

    pub(super) fn modules(
        &mut self,
        key: ProcKey,
        addresses: &[u64],
        tids: &[u32],
    ) -> ModuleReport {
        let started = Instant::now();
        let (modules, error) = match crate::modules::process_modules(key.pid) {
            Ok(m) => (m, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let symbols = addresses
            .iter()
            .filter(|&&a| a != 0)
            .filter_map(|&a| Some((a, self.symbols.name(&modules, a)?)))
            .collect();
        ModuleReport {
            key,
            modules: modules
                .into_iter()
                // What the file is (its package, version) is not read yet.
                .map(|info| ModuleRow {
                    info,
                    description: None,
                    company: None,
                    version: None,
                })
                .collect(),
            error,
            symbols,
            thread_names: crate::threads::names(key.pid, tids),
            took: started.elapsed(),
        }
    }

    pub(super) fn stack(
        &mut self,
        key: ProcKey,
        tid: u32,
    ) -> Result<crate::threads::StackReport, String> {
        crate::threads::call_stack(key, tid, &mut self.symbols)
    }

    /// Every open file and mapped file (what `lsof` lists) whose path holds
    /// `query`, case aside, in every process but those named in `exclude`.
    pub(super) fn find(&mut self, query: &str, exclude: &[String]) -> Result<FindReport, String> {
        let started = Instant::now();
        let wanted = query.to_lowercase();
        if wanted.is_empty() {
            return Err("nothing to look for".into());
        }
        let mut report = FindReport {
            query: query.to_string(),
            ..Default::default()
        };
        let own = std::process::id();
        let mapped: Arc<str> = Arc::from("Mapped file");
        let type_names: Vec<Arc<str>> = TYPES.iter().map(|t| Arc::from(*t)).collect();
        'processes: for pid in procfs::pids() {
            let Some(stat) = procfs::stat(pid) else {
                continue;
            };
            if pid == own || stat.kernel_thread() {
                continue;
            }
            if exclude.iter().any(|x| x.eq_ignore_ascii_case(&stat.comm)) {
                continue;
            }
            let process: Arc<str> = Arc::from(stat.comm.as_str());
            let dir = match std::fs::read_dir(format!("/proc/{pid}/fd")) {
                Ok(dir) => dir,
                Err(e) => {
                    report.skipped.push((process, pid, unreadable(&e)));
                    continue;
                }
            };
            report.processes += 1;
            let mut found = std::collections::HashSet::new();
            for e in dir.flatten() {
                report.handles += 1;
                let Ok(target) = std::fs::read_link(e.path()) else {
                    continue;
                };
                let target = target.to_string_lossy();
                if !target.to_lowercase().contains(&wanted) {
                    continue;
                }
                let directory = std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir());
                report.matches.push(FoundHandle {
                    pid,
                    process: process.clone(),
                    handle: e
                        .file_name()
                        .to_str()
                        .and_then(|f| f.parse().ok())
                        .unwrap_or(0),
                    type_name: type_names[classify(&target, directory) as usize].clone(),
                    name: target.to_string(),
                });
                found.insert(target.into_owned());
                if report.matches.len() >= MAX_MATCHES {
                    break 'processes;
                }
            }
            // Libraries and other files mapped into memory, once each.
            for m in crate::ptrace::maps(pid).unwrap_or_default() {
                if m.file_backed()
                    && m.path.to_lowercase().contains(&wanted)
                    && found.insert(m.path.clone())
                {
                    report.matches.push(FoundHandle {
                        pid,
                        process: process.clone(),
                        handle: m.start,
                        type_name: mapped.clone(),
                        name: m.path,
                    });
                }
            }
            if report.matches.len() >= MAX_MATCHES {
                break;
            }
        }
        report.took = started.elapsed();
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_its_own_open_files() {
        let file = std::fs::File::open("/proc/self/status").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let me = procfs::stat(std::process::id()).unwrap();
        let key = ProcKey {
            pid: std::process::id(),
            create_time: me.create_time(),
        };
        let report = handles_now(key, true).unwrap();
        assert_eq!(report.resolved, report.entries.len());
        assert!(report.system_total >= report.entries.len());
        let named = |text: &str| {
            report
                .entries
                .iter()
                .zip(&report.names)
                .find(|(_, n)| n.as_deref().is_some_and(|n| n.contains(text)))
                .map(|(e, _)| *e)
        };
        let socket = named(&format!(":{port}")).expect("the listener is named by its port");
        assert_eq!(socket.type_index, SOCKET);
        assert_eq!(report.type_name(socket.type_index).as_ref(), "Socket");
        let status = named("/status").expect("the open file is named by its path");
        assert_eq!(status.type_index, FILE);
        assert_eq!(status.access, 1, "opened for reading only");
        drop((file, listener));
    }

    #[test]
    fn finds_a_file_a_child_holds() {
        let dir = std::env::temp_dir().join(format!("anabolic-find-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("held.txt");
        std::fs::write(&path, b"x").unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::fs::File::open(&path).unwrap())
            .spawn()
            .unwrap();
        let report = Bench::new().find("HELD.TXT", &[]).unwrap();
        let object = std::fs::metadata(&path).map(|m| object_id(&m)).unwrap();
        let holders = holders_of(object, &[std::process::id()]).unwrap();
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            report
                .matches
                .iter()
                .any(|m| m.pid == child.id() && m.handle == 0),
            "{:?}",
            report.matches
        );
        assert!(
            holders
                .iter()
                .any(|(pid, _, fds, _)| *pid == child.id() && fds == &[0])
        );
    }

    #[test]
    fn counts_zombies_by_parent() {
        // A child that has ended and is not collected yet.
        let child = std::process::Command::new("true").spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let report = zombie_report().unwrap();
        let mine = report
            .holders
            .iter()
            .find(|h| h.pid == std::process::id())
            .map_or(0, |h| h.zombies);
        drop(child);
        assert!(mine >= 1);
        assert!(report.process_objects as usize >= report.running + report.zombies);
    }
}
