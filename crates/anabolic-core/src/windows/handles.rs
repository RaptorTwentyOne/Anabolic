//! Handle inspection.
//!
//! Counting reads the kernel's system-wide handle table, so the inspected
//! process is never opened (games with anti-cheat stay untouched). Resolving
//! names is opt-in: every handle has to be duplicated into Anabolic, which
//! needs PROCESS_DUP_HANDLE on the target.

use core::ffi::c_void;
use core::mem::size_of;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use windows::Win32::Foundation::{DUPLICATE_HANDLE_OPTIONS, DuplicateHandle, HANDLE, STILL_ACTIVE};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, GetExitCodeThread, GetProcessId, GetProcessIdOfThread,
    GetThreadId, OpenProcess, PROCESS_DUP_HANDLE, PROCESS_QUERY_LIMITED_INFORMATION,
    THREAD_QUERY_LIMITED_INFORMATION,
};

use windows::Win32::Storage::FileSystem::{FILE_TYPE_DISK, GetFileType};

use crate::model::ProcKey;
use crate::modules::{Symbolizer, kernel_modules, process_modules};
use crate::ntapi::{
    self, OBJECT_TYPE_INFORMATION, SYSTEM_HANDLE_INFORMATION_EX, SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX,
};
use crate::util::{OwnedHandle, error_text};
use crate::winfile::DeviceMap;

use super::*;

/// Object type index -> name and system-wide object/handle counts.
pub fn object_types() -> HashMap<u16, ObjectTypeInfo> {
    let mut map = HashMap::new();
    let mut buf: Vec<u64> = vec![0; 64 * 1024 / 8];
    for _ in 0..4 {
        let mut needed = 0u32;
        let status = unsafe {
            ntapi::NtQueryObject(
                HANDLE::default(),
                ntapi::ObjectTypesInformation,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                &mut needed,
            )
        };
        if status == ntapi::STATUS_INFO_LENGTH_MISMATCH {
            buf.resize((needed as usize).max(buf.len() * 16).div_ceil(8), 0);
            continue;
        }
        if !ntapi::nt_success(status) {
            return map;
        }
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len() * 8) };
        let count = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        let mut at = 8; // header rounded up to pointer size
        for i in 0..count {
            if at + size_of::<OBJECT_TYPE_INFORMATION>() > bytes.len() {
                break;
            }
            let info = unsafe {
                bytes
                    .as_ptr()
                    .add(at)
                    .cast::<OBJECT_TYPE_INFORMATION>()
                    .read_unaligned()
            };
            let name = unicode_in(bytes, info.TypeName).unwrap_or_default();
            // Before Windows 8.1 the index is implicit: types start at 2.
            let index = if info.TypeIndex != 0 {
                info.TypeIndex as u16
            } else {
                i as u16 + 2
            };
            map.insert(
                index,
                ObjectTypeInfo {
                    name: Arc::from(name),
                    objects: info.TotalNumberOfObjects,
                    handles: info.TotalNumberOfHandles,
                },
            );
            at += size_of::<OBJECT_TYPE_INFORMATION>()
                + (info.TypeName.MaximumLength as usize).next_multiple_of(8);
        }
        return map;
    }
    map
}

/// Reads a UNICODE_STRING whose buffer points into `bytes`.
fn unicode_in(bytes: &[u8], s: ntapi::UNICODE_STRING) -> Option<String> {
    let base = bytes.as_ptr() as usize;
    let ptr = s.Buffer as usize;
    let len = s.Length as usize & !1;
    if s.Buffer.is_null() || ptr < base || ptr + len > base + bytes.len() {
        return None;
    }
    let at = ptr - base;
    let units: Vec<u16> = bytes[at..at + len]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// Runs `f` over the kernel's system-wide handle table (one query) and returns
/// its result together with the number of handles open system-wide.
pub fn with_handle_table<R>(
    f: impl FnOnce(&[SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX]) -> R,
) -> Result<(R, usize), String> {
    let mut buf: Vec<u64> = vec![0; 16 * 1024 * 1024 / 8];
    loop {
        let mut needed = 0u32;
        let bytes = buf.len() * 8;
        let status = unsafe {
            ntapi::NtQuerySystemInformation(
                ntapi::SystemExtendedHandleInformation,
                buf.as_mut_ptr().cast(),
                bytes as u32,
                &mut needed,
            )
        };
        if status == ntapi::STATUS_INFO_LENGTH_MISMATCH {
            // Handles come and go; leave headroom for the retry.
            let want = (needed as usize).max(bytes) + 4 * 1024 * 1024;
            if want > 1024 * 1024 * 1024 {
                return Err("system handle table is too large".into());
            }
            buf = vec![0; want.div_ceil(8)];
            continue;
        }
        if !ntapi::nt_success(status) {
            return Err(ntapi::status_message(status));
        }
        let header = unsafe { &*buf.as_ptr().cast::<SYSTEM_HANDLE_INFORMATION_EX>() };
        let first = size_of::<SYSTEM_HANDLE_INFORMATION_EX>();
        let fits = (bytes - first) / size_of::<SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX>();
        let count = header.NumberOfHandles.min(fits);
        // SAFETY: in bounds (count clamped to the buffer) and 8-byte aligned.
        let table = unsafe {
            std::slice::from_raw_parts(
                buf.as_ptr()
                    .cast::<u8>()
                    .add(first)
                    .cast::<SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX>(),
                count,
            )
        };
        return Ok((f(table), count));
    }
}

fn entry(h: &SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX) -> HandleEntry {
    HandleEntry {
        value: h.HandleValue as u64,
        type_index: h.ObjectTypeIndex,
        access: h.GrantedAccess,
        attributes: h.HandleAttributes,
        object: h.Object as u64,
    }
}

/// Every handle `pid` holds, plus the number of handles open system-wide.
pub fn process_handles(pid: u32) -> Result<(Vec<HandleEntry>, usize), String> {
    with_handle_table(|table| {
        table
            .iter()
            .filter(|h| h.UniqueProcessId == pid as usize)
            .map(entry)
            .collect()
    })
}

/// A handle to every running process, held so that their kernel objects show
/// up in our own handle table; that maps object addresses back to PIDs.
pub struct LiveProcesses {
    handles: Vec<(u32, OwnedHandle)>,
}

impl LiveProcesses {
    pub fn open_all() -> Self {
        let mut scanner = crate::scan::ProcessScanner::new();
        let pids: Vec<u32> = match scanner.refresh() {
            Ok(()) => scanner
                .entries()
                .map(|e| e.pid())
                .filter(|&p| p != 0)
                .collect(),
            Err(_) => Vec::new(),
        };
        let handles = pids
            .into_iter()
            .filter_map(|pid| {
                let h =
                    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
                Some((pid, OwnedHandle(h)))
            })
            .collect();
        Self { handles }
    }

    pub fn len(&self) -> usize {
        self.handles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }

    /// Kernel object address -> PID, from our own entries in a handle table
    /// taken while `self` was alive.
    pub fn object_map(&self, table: &[SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX]) -> HashMap<u64, u32> {
        let own = std::process::id() as usize;
        let by_value: HashMap<usize, u32> = self
            .handles
            .iter()
            .map(|(pid, h)| (h.raw().0 as usize, *pid))
            .collect();
        table
            .iter()
            .filter(|h| h.UniqueProcessId == own && h.Object != 0)
            .filter_map(|h| Some((h.Object as u64, *by_value.get(&h.HandleValue)?)))
            .collect()
    }
}

fn process_targets(
    entries: &[HandleEntry],
    process_type: Option<u16>,
    live: &HashMap<u64, u32>,
) -> Option<ProcessTargets> {
    let process_type = process_type?;
    let objects: Vec<u64> = entries
        .iter()
        .filter(|e| e.type_index == process_type)
        .map(|e| e.object)
        .collect();
    // Kernel addresses are hidden from callers without SeDebugPrivilege.
    if objects.is_empty() || live.is_empty() || objects.iter().all(|&o| o == 0) {
        return None;
    }
    let mut per_object: HashMap<u64, usize> = HashMap::new();
    for &o in &objects {
        *per_object.entry(o).or_default() += 1;
    }
    let mut t = ProcessTargets {
        handles: objects.len(),
        distinct: per_object.len(),
        ..Default::default()
    };
    let mut per_pid: HashMap<u32, usize> = HashMap::new();
    for (object, count) in per_object {
        match live.get(&object) {
            Some(&pid) => {
                t.running += 1;
                *per_pid.entry(pid).or_default() += count;
            }
            None => {
                t.exited += 1;
                t.handles_to_exited += count;
            }
        }
    }
    t.top_running = per_pid.into_iter().collect();
    t.top_running.sort_by_key(|x| std::cmp::Reverse(x.1));
    t.top_running.truncate(10);
    Some(t)
}

pub fn zombie_report() -> Result<ZombieReport, String> {
    let started = Instant::now();
    let types = object_types();
    let process_type = types
        .iter()
        .find(|(_, t)| &*t.name == "Process")
        .map(|(i, _)| *i)
        .ok_or("no Process object type")?;
    let live = LiveProcesses::open_all();
    let own = std::process::id() as usize;
    let ((zombies, holders), _) = with_handle_table(|table| {
        let map = live.object_map(table);
        let mut all_dead: HashSet<u64> = HashSet::new();
        let mut per_holder: HashMap<u32, (HashSet<u64>, usize)> = HashMap::new();
        for h in table {
            if h.ObjectTypeIndex != process_type || h.Object == 0 || h.UniqueProcessId == own {
                continue;
            }
            let object = h.Object as u64;
            if map.contains_key(&object) {
                continue;
            }
            all_dead.insert(object);
            let slot = per_holder.entry(h.UniqueProcessId as u32).or_default();
            slot.0.insert(object);
            slot.1 += 1;
        }
        let mut holders: Vec<ZombieHolder> = per_holder
            .into_iter()
            .map(|(pid, (objects, handles))| ZombieHolder {
                pid,
                zombies: objects.len(),
                handles,
            })
            .collect();
        holders.sort_by_key(|h| std::cmp::Reverse(h.zombies));
        (all_dead.len(), holders)
    })?;
    Ok(ZombieReport {
        process_objects: types[&process_type].objects,
        running: live.len(),
        zombies,
        holders,
        took: started.elapsed(),
    })
}

fn object_name(handle: HANDLE, buf: &mut Vec<u64>) -> Option<String> {
    for _ in 0..3 {
        let mut needed = 0u32;
        let status = unsafe {
            ntapi::NtQueryObject(
                handle,
                ntapi::ObjectNameInformation,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                &mut needed,
            )
        };
        if ntapi::nt_success(status) {
            let bytes =
                unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len() * 8) };
            let name = unsafe { buf.as_ptr().cast::<ntapi::UNICODE_STRING>().read() };
            return unicode_in(bytes, name).filter(|s| !s.is_empty());
        }
        let grow = matches!(
            status,
            ntapi::STATUS_INFO_LENGTH_MISMATCH | ntapi::STATUS_BUFFER_OVERFLOW
        );
        if !grow || needed as usize <= buf.len() * 8 || needed > 1 << 16 {
            return None;
        }
        buf.resize((needed as usize).div_ceil(8), 0);
    }
    None
}

/// Queries file object names on a helper thread. A synchronous file (a pipe
/// with a pending read, typically) makes NtQueryObject block forever; when
/// that happens the helper is abandoned and a fresh one takes over.
struct FileNamer {
    tx: Sender<usize>,
    rx: Receiver<Option<String>>,
    abandoned: u32,
}

impl FileNamer {
    const TIMEOUT: Duration = Duration::from_millis(60);
    const MAX_ABANDONED: u32 = 32;

    fn new() -> Self {
        let (tx, requests) = unbounded::<usize>();
        let (results, rx) = bounded::<Option<String>>(1);
        thread::Builder::new()
            .name("anabolic-filenamer".into())
            .spawn(move || {
                let mut buf = vec![0u64; 1024 / 8];
                for raw in requests {
                    let handle = OwnedHandle(HANDLE(raw as *mut c_void));
                    // Pipes and console handles are where name queries hang;
                    // only files on disk have names worth asking for.
                    let name = match unsafe { GetFileType(handle.raw()) } {
                        FILE_TYPE_DISK => object_name(handle.raw(), &mut buf),
                        _ => None,
                    };
                    if results.send(name).is_err() {
                        return;
                    }
                }
            })
            .expect("failed to spawn the file name thread");
        Self {
            tx,
            rx,
            abandoned: 0,
        }
    }

    /// Takes ownership of `handle`. `Err` means the query hung.
    fn name(&mut self, handle: OwnedHandle) -> Result<Option<String>, ()> {
        if self.abandoned >= Self::MAX_ABANDONED {
            return Err(());
        }
        let raw = handle.raw().0 as usize;
        std::mem::forget(handle); // the helper closes it
        if self.tx.send(raw).is_err() {
            return Err(());
        }
        match self.rx.recv_timeout(Self::TIMEOUT) {
            Ok(name) => Ok(name),
            Err(_) => {
                let abandoned = self.abandoned + 1;
                *self = FileNamer::new();
                self.abandoned = abandoned;
                Err(())
            }
        }
    }
}

fn prettify_key(name: String) -> String {
    for (nt, short) in [(r"\REGISTRY\MACHINE", "HKLM"), (r"\REGISTRY\USER", "HKU")] {
        if let Some(rest) = name.strip_prefix(nt) {
            return format!("{short}{rest}");
        }
    }
    name
}

/// Names handles of one process after another, reusing the file name
/// helper (and its hang protection) across them.
struct Namer {
    files: FileNamer,
    buf: Vec<u64>,
}

impl Namer {
    fn new() -> Self {
        Self {
            files: FileNamer::new(),
            buf: vec![0u64; 1024 / 8],
        }
    }

    /// Name (or target, for process and thread handles) of one handle of the
    /// process `target` was opened for with PROCESS_DUP_HANDLE.
    fn name(
        &mut self,
        target: &OwnedHandle,
        e: &HandleEntry,
        kind: &str,
        devices: &DeviceMap,
    ) -> Option<String> {
        let me = unsafe { GetCurrentProcess() };
        let duplicate = |access: u32| -> Option<OwnedHandle> {
            let mut dup = HANDLE::default();
            unsafe {
                DuplicateHandle(
                    target.raw(),
                    HANDLE(e.value as *mut c_void),
                    me,
                    &mut dup,
                    access,
                    false,
                    DUPLICATE_HANDLE_OPTIONS(0),
                )
            }
            .ok()?;
            Some(OwnedHandle(dup))
        };
        match kind {
            "Process" => duplicate(PROCESS_QUERY_LIMITED_INFORMATION.0).map(|h| {
                let pid = unsafe { GetProcessId(h.raw()) };
                let mut code = 0u32;
                let exited = unsafe { GetExitCodeProcess(h.raw(), &mut code) }.is_ok()
                    && code != STILL_ACTIVE.0 as u32;
                format!("PID {pid}{}", if exited { " (exited)" } else { "" })
            }),
            "Thread" => duplicate(THREAD_QUERY_LIMITED_INFORMATION.0).map(|h| {
                let (tid, owner) = unsafe { (GetThreadId(h.raw()), GetProcessIdOfThread(h.raw())) };
                let mut code = 0u32;
                let exited = unsafe { GetExitCodeThread(h.raw(), &mut code) }.is_ok()
                    && code != STILL_ACTIVE.0 as u32;
                format!(
                    "TID {tid} in PID {owner}{}",
                    if exited { " (exited)" } else { "" }
                )
            }),
            "File" => match duplicate(0) {
                Some(h) => match self.files.name(h) {
                    Ok(name) => name.map(|n| devices.to_dos_path(&n).unwrap_or(n)),
                    Err(()) => Some("<query timed out>".to_string()),
                },
                None => None,
            },
            _ => duplicate(0)
                .and_then(|h| object_name(h.raw(), &mut self.buf))
                .map(|n| if kind == "Key" { prettify_key(n) } else { n }),
        }
    }
}

/// Names (or targets, for process and thread handles) of the first `limit`
/// entries. Process/thread handles pointing at something that already exited
/// are marked, since those are the classic leak.
pub fn resolve_names(
    pid: u32,
    entries: &[HandleEntry],
    types: &HashMap<u16, Arc<str>>,
    devices: &DeviceMap,
    limit: usize,
) -> Result<Vec<Option<Arc<str>>>, String> {
    let target = OwnedHandle(
        unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, pid) }.map_err(|e| error_text(&e))?,
    );
    let mut namer = Namer::new();
    let mut cache: HashMap<String, Arc<str>> = HashMap::new();
    Ok(entries
        .iter()
        .take(limit)
        .map(|e| {
            let kind = types.get(&e.type_index).map_or("", |s| s);
            namer.name(&target, e, kind, devices).map(|s| {
                cache
                    .entry(s.clone())
                    .or_insert_with(|| Arc::from(s))
                    .clone()
            })
        })
        .collect())
}

/// Object types whose handles carry a name worth searching.
const NAMED_TYPES: [&str; 11] = [
    "File",
    "Key",
    "Section",
    "Mutant",
    "Event",
    "Semaphore",
    "Timer",
    "Directory",
    "SymbolicLink",
    "Job",
    "ALPC Port",
];
const MAX_MATCHES: usize = 5_000;

/// Every handle in the system whose name contains `query` (case-insensitive),
/// e.g. the file a process keeps locked. Duplicates handles out of every
/// process except those named in `exclude`.
pub fn find_handles(
    query: &str,
    exclude: &[String],
    types: &HashMap<u16, Arc<str>>,
    devices: &DeviceMap,
) -> Result<FindReport, String> {
    let started = Instant::now();
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Err("nothing to search for".into());
    }
    let named: HashSet<u16> = types
        .iter()
        .filter(|(_, n)| NAMED_TYPES.contains(&n.as_ref()))
        .map(|(&i, _)| i)
        .collect();

    let mut scanner = crate::scan::ProcessScanner::new();
    scanner.refresh().map_err(ntapi::status_message)?;
    let names: HashMap<u32, Arc<str>> = scanner
        .entries()
        .map(|e| {
            let image = String::from_utf16_lossy(&e.image_name());
            (e.pid(), Arc::from(image.rsplit('\\').next().unwrap_or("")))
        })
        .collect();

    let own = std::process::id();
    let (per_pid, _) = with_handle_table(|table| {
        let mut per_pid: HashMap<u32, Vec<HandleEntry>> = HashMap::new();
        for h in table {
            if named.contains(&h.ObjectTypeIndex) {
                per_pid
                    .entry(h.UniqueProcessId as u32)
                    .or_default()
                    .push(entry(h));
            }
        }
        per_pid
    })?;

    let mut report = FindReport {
        query: query.to_string(),
        ..Default::default()
    };
    let mut namer = Namer::new();
    let mut pids: Vec<u32> = per_pid.keys().copied().collect();
    pids.sort_unstable();
    for pid in pids {
        let process = names.get(&pid).cloned().unwrap_or_else(|| Arc::from("?"));
        if pid == own || pid == 0 {
            continue;
        }
        if exclude.iter().any(|x| x.eq_ignore_ascii_case(&process)) {
            report.skipped.push((process, pid, "excluded".into()));
            continue;
        }
        let target = match unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, pid) } {
            Ok(h) => OwnedHandle(h),
            Err(e) => {
                report.skipped.push((process, pid, error_text(&e)));
                continue;
            }
        };
        report.processes += 1;
        for e in &per_pid[&pid] {
            report.handles += 1;
            let kind = types
                .get(&e.type_index)
                .cloned()
                .unwrap_or_else(|| Arc::from("?"));
            if let Some(name) = namer.name(&target, e, &kind, devices)
                && name.to_lowercase().contains(&needle)
            {
                report.matches.push(FoundHandle {
                    pid,
                    process: process.clone(),
                    handle: e.value,
                    type_name: kind,
                    name,
                });
                if report.matches.len() >= MAX_MATCHES {
                    report.took = started.elapsed();
                    return Ok(report);
                }
            }
        }
    }
    report.took = started.elapsed();
    Ok(report)
}

/// Addresses at or above this are kernel mode.
const KERNEL_SPACE: u64 = 0xFFFF_8000_0000_0000;

struct ModuleInspector {
    symbols: Symbolizer,
    files: HashMap<Arc<str>, Arc<crate::winfile::FileInfo>>,
}

impl ModuleInspector {
    fn inspect(
        &mut self,
        key: ProcKey,
        addresses: &[u64],
        tids: &[u32],
        devices: &DeviceMap,
    ) -> ModuleReport {
        let started = Instant::now();
        let kernel = kernel_modules(devices);
        let (modules, error) = if key.pid == 4 {
            (kernel.clone(), None)
        } else {
            match process_modules(key.pid) {
                Ok(m) => (m, None),
                Err(e) => (Vec::new(), Some(e)),
            }
        };
        let mut symbols = HashMap::new();
        for &address in addresses {
            let list = if address >= KERNEL_SPACE {
                &kernel
            } else {
                &modules
            };
            if let Some(name) = self.symbols.name(list, address) {
                symbols.insert(address, name);
            }
        }
        let modules = modules
            .into_iter()
            .map(|info| {
                let file = self
                    .files
                    .entry(info.path.clone())
                    .or_insert_with(|| Arc::new(crate::winfile::file_info(&info.path)))
                    .clone();
                ModuleRow {
                    info,
                    description: file.description.clone(),
                    company: file.company.clone(),
                    version: file.version.clone(),
                }
            })
            .collect();
        ModuleReport {
            key,
            modules,
            error,
            symbols,
            thread_names: crate::threads::names(key.pid, tids),
            took: started.elapsed(),
        }
    }
}

const NAME_LIMIT: usize = 250_000;

/// What the inspector thread keeps between requests: object type names,
/// the device map and module facts are slow to read.
pub(super) struct Bench {
    types: HashMap<u16, Arc<str>>,
    devices: DeviceMap,
    modules: ModuleInspector,
}

impl Bench {
    pub(super) fn new() -> Self {
        Self {
            types: object_type_names(),
            devices: DeviceMap::build(),
            modules: ModuleInspector {
                symbols: Symbolizer::default(),
                files: HashMap::new(),
            },
        }
    }

    pub(super) fn handles(&mut self, key: ProcKey, names: bool) -> Result<HandleReport, String> {
        inspect_handles(key, names, &mut self.types, &self.devices)
    }

    pub(super) fn modules(
        &mut self,
        key: ProcKey,
        addresses: &[u64],
        tids: &[u32],
    ) -> ModuleReport {
        self.modules.inspect(key, addresses, tids, &self.devices)
    }

    pub(super) fn stack(
        &mut self,
        key: ProcKey,
        tid: u32,
    ) -> Result<crate::threads::StackReport, String> {
        crate::threads::call_stack(key, tid, &mut self.modules.symbols)
    }

    pub(super) fn find(&mut self, query: &str, exclude: &[String]) -> Result<FindReport, String> {
        find_handles(query, exclude, &self.types, &self.devices)
    }
}

/// Live processes plus exited ones somebody still holds a handle to.
pub fn process_objects() -> u32 {
    object_types()
        .into_values()
        .find(|t| &*t.name == "Process")
        .map_or(0, |t| t.objects)
}

/// The handles of one process, read on the calling thread (the MCP
/// server's), with names when `names` (which duplicates handles out of it).
pub fn handles_now(key: ProcKey, names: bool) -> Result<HandleReport, String> {
    inspect_handles(key, names, &mut HashMap::new(), &DeviceMap::build())
}

fn inspect_handles(
    key: ProcKey,
    want_names: bool,
    types: &mut HashMap<u16, Arc<str>>,
    devices: &DeviceMap,
) -> Result<HandleReport, String> {
    let started = Instant::now();
    let live = LiveProcesses::open_all();
    let ((entries, live_map), system_total) = with_handle_table(|table| {
        let entries: Vec<HandleEntry> = table
            .iter()
            .filter(|h| h.UniqueProcessId == key.pid as usize)
            .map(entry)
            .collect();
        (entries, live.object_map(table))
    })?;
    drop(live);
    if entries.iter().any(|e| !types.contains_key(&e.type_index)) {
        *types = object_type_names();
    }
    let process_type = types
        .iter()
        .find(|(_, n)| &***n == "Process")
        .map(|(i, _)| *i);
    let (names, name_error) = if want_names {
        match resolve_names(key.pid, &entries, types, devices, NAME_LIMIT) {
            Ok(names) => (names, None),
            Err(e) => (Vec::new(), Some(e)),
        }
    } else {
        (Vec::new(), None)
    };
    Ok(HandleReport {
        key,
        by_type: count_by_type(&entries, types),
        type_names: types.clone(),
        resolved: names.len(),
        name_groups: name_groups(&entries, &names),
        names,
        name_error,
        process_targets: process_targets(&entries, process_type, &live_map),
        entries,
        system_total,
        took: started.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_handles_have_known_types() {
        let types = object_type_names();
        assert!(types.values().any(|n| &**n == "File"));
        assert!(types.values().any(|n| &**n == "Event"));
        let (entries, total) = process_handles(std::process::id()).unwrap();
        assert!(!entries.is_empty() && total > entries.len());
        let by_type = count_by_type(&entries, &types);
        assert_eq!(
            by_type.iter().map(|t| t.count as usize).sum::<usize>(),
            entries.len()
        );
        assert!(
            by_type.iter().all(|t| !t.name.starts_with("Type #")),
            "{by_type:?}"
        );
    }

    #[test]
    fn maps_live_process_objects() {
        let live = LiveProcesses::open_all();
        assert!(live.len() > 10);
        let (map, _) = with_handle_table(|t| live.object_map(t)).unwrap();
        // Kernel addresses need SeDebugPrivilege; only check when visible.
        if !map.is_empty() {
            assert!(map.values().any(|&p| p == std::process::id()));
        }
    }

    #[test]
    fn zombie_report_is_consistent() {
        let report = zombie_report().unwrap();
        assert!(report.running > 10);
        assert!(
            report.process_objects as usize + 50 >= report.running,
            "{report:?}"
        );
        assert!(
            report
                .holders
                .iter()
                .all(|h| h.zombies > 0 && h.handles >= h.zombies)
        );
        assert!(report.holders.iter().map(|h| h.zombies).max().unwrap_or(0) <= report.zombies);
    }

    #[test]
    fn finds_a_file_another_process_holds() {
        let _ = crate::hardening::enable_privilege(windows::core::w!("SeDebugPrivilege"));
        let dir = std::env::temp_dir();
        let marker = format!("anabolic-lock-{}.txt", std::process::id());
        let path = dir.join(&marker);
        std::fs::write(&path, b"held").unwrap();
        // The child inherits the open file as its stdin and sits on it.
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "ping -n 6 127.0.0.1 >nul"])
            .stdin(std::fs::File::open(&path).unwrap())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let types = object_type_names();
        let report = find_handles(&marker, &[], &types, &DeviceMap::build()).unwrap();
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&path);
        assert!(
            report
                .matches
                .iter()
                .any(|m| m.pid == child.id() && m.name.ends_with(&marker)),
            "{} processes, {} handles, matches {:?}",
            report.processes,
            report.handles,
            report.matches
        );
    }

    #[test]
    fn resolves_own_file_and_key_names() {
        let types = object_type_names();
        let (entries, _) = process_handles(std::process::id()).unwrap();
        let names = resolve_names(
            std::process::id(),
            &entries,
            &types,
            &DeviceMap::build(),
            usize::MAX,
        )
        .unwrap();
        assert_eq!(names.len(), entries.len());
        let named: Vec<_> = entries
            .iter()
            .zip(&names)
            .filter_map(|(e, n)| Some((types[&e.type_index].clone(), n.clone()?)))
            .collect();
        assert!(
            named
                .iter()
                .any(|(t, n)| &**t == "Key" && n.starts_with("HK")),
            "{named:?}"
        );
        assert!(
            named.iter().any(|(t, n)| &**t == "File" && n.contains(':')),
            "{named:?}"
        );
    }
}
