//! Why a program does not respond.
//!
//! - [`hung_windows`]: the top-level windows Windows itself considers not
//!   responding (their thread has taken no message for five seconds). It
//!   reads window state only and touches no process, so it runs every tick.
//! - [`wait_chains`]: what a process' threads are waiting for, link by
//!   link and across processes (Wait Chain Traversal, the engine behind
//!   Resource Monitor's "Analyze wait chain"). It opens the threads, so it
//!   runs only when asked.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Diagnostics::Debug::{
    CloseThreadWaitChainSession, GetThreadWaitChain, OPEN_THREAD_WAIT_CHAIN_SESSION_FLAGS,
    OpenThreadWaitChainSession, RegisterWaitChainCOMCallback, WAIT_CHAIN_THREAD_OPTIONS,
    WAITCHAIN_NODE_INFO, WCT_MAX_NODE_COUNT, WCT_NETWORK_IO_FLAG, WCT_OBJECT_STATUS,
    WCT_OBJECT_TYPE, WCT_OUT_OF_PROC_COM_FLAG, WCT_OUT_OF_PROC_CS_FLAG, WCT_OUT_OF_PROC_FLAG,
    WctAlpcType, WctComActivationType, WctComType, WctCriticalSectionType, WctMutexType,
    WctProcessWaitType, WctSendMessageType, WctSmbIoType, WctSocketIoType, WctStatusAbandoned,
    WctStatusBlocked, WctStatusError, WctStatusNoAccess, WctStatusNotOwned, WctStatusOwned,
    WctStatusPidOnly, WctStatusPidOnlyRpcss, WctStatusRunning, WctThreadType, WctThreadWaitType,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_STYLE, GetWindow, GetWindowLongPtrW, GetWindowTextW,
    GetWindowThreadProcessId, IsHungAppWindow, IsWindowVisible, WS_CAPTION,
};
use windows::core::{BOOL, s, w};

use crate::model::ProcKey;
use crate::util::from_wide;

use super::*;

impl Wait {
    #[allow(non_upper_case_globals)]
    fn from(kind: WCT_OBJECT_TYPE) -> Self {
        match kind {
            WctCriticalSectionType => Wait::CriticalSection,
            WctSendMessageType => Wait::SendMessage,
            WctMutexType => Wait::Mutex,
            WctAlpcType => Wait::Alpc,
            WctComType => Wait::Com,
            WctThreadWaitType => Wait::Thread,
            WctProcessWaitType => Wait::Process,
            WctComActivationType => Wait::ComActivation,
            WctSocketIoType => Wait::Socket,
            WctSmbIoType => Wait::Smb,
            _ => Wait::Unknown,
        }
    }
}

impl Status {
    // The windows crate keeps the SDK's names for these constants.
    #[allow(non_upper_case_globals)]
    fn from(status: WCT_OBJECT_STATUS) -> Self {
        match status {
            WctStatusRunning => Status::Running,
            WctStatusBlocked => Status::Blocked,
            WctStatusPidOnly | WctStatusPidOnlyRpcss => Status::PidOnly,
            WctStatusOwned => Status::Owned,
            WctStatusNotOwned => Status::NotOwned,
            WctStatusAbandoned => Status::Abandoned,
            WctStatusNoAccess => Status::NoAccess,
            WctStatusError => Status::Error,
            _ => Status::Unknown,
        }
    }
}

/// Application windows Windows marks as not responding, one per thread.
pub fn hung_windows() -> Vec<HungWindow> {
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let found = unsafe { &mut *(lparam.0 as *mut Vec<HungWindow>) };
        // Unowned windows with a caption. A hung window is hidden behind
        // DWM's ghost copy of it, so "visible" alone would miss it.
        if unsafe { GetWindow(hwnd, GW_OWNER) }.is_ok_and(|o| !o.is_invalid()) {
            return BOOL(1);
        }
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
        let captioned = style & WS_CAPTION.0 == WS_CAPTION.0;
        if !captioned && !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            return BOOL(1);
        }
        if !unsafe { IsHungAppWindow(hwnd) }.as_bool() {
            return BOOL(1);
        }
        let mut title = [0u16; 256];
        let len = unsafe { GetWindowTextW(hwnd, &mut title) } as usize;
        if len == 0 {
            return BOOL(1);
        }
        let mut pid = 0u32;
        let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if !found.iter().any(|h| h.tid == tid) {
            found.push(HungWindow {
                pid,
                tid,
                title: String::from_utf16_lossy(&title[..len]),
            });
        }
        BOOL(1)
    }
    let mut found = Vec::new();
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&raw mut found as isize)) };
    found
}

/// The COM callbacks let a chain continue into the COM server it calls.
fn com_callbacks() -> bool {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    *REGISTERED.get_or_init(|| unsafe {
        let Ok(ole) = LoadLibraryW(w!("ole32.dll")) else {
            return false;
        };
        match (
            GetProcAddress(ole, s!("CoGetCallState")),
            GetProcAddress(ole, s!("CoGetActivationState")),
        ) {
            (Some(call), Some(activation)) => {
                RegisterWaitChainCOMCallback(
                    Some(std::mem::transmute::<
                        unsafe extern "system" fn() -> isize,
                        unsafe extern "system" fn(i32, *mut u32) -> windows::core::HRESULT,
                    >(call)),
                    Some(std::mem::transmute::<
                        unsafe extern "system" fn() -> isize,
                        unsafe extern "system" fn(
                            windows::core::GUID,
                            u32,
                            *mut u32,
                        )
                            -> windows::core::HRESULT,
                    >(activation)),
                );
                true
            }
            _ => false,
        }
    })
}

/// Waits no chain can lead out of: a thread pool thread waiting for work
/// (WrQueue), a sleep, a suspension.
const IDLE_WAITS: [u32; 5] = [
    4,  // DelayExecution
    5,  // Suspended
    11, // WrDelayExecution
    12, // WrSuspended
    15, // WrQueue
];

/// The threads of a process worth asking about, and how many were left
/// out: running threads wait for nothing, and idle waits lead nowhere.
/// Windows spends some 50 ms of CPU on every thread it is asked about, so
/// in a process of hundreds of threads this saves seconds. `first` is kept
/// whatever it does. From the system's thread list: the process itself is
/// not opened for this.
fn threads_of(pid: u32, first: Option<u32>) -> Result<(Vec<u32>, usize), String> {
    let mut scanner = crate::scan::ProcessScanner::new();
    scanner
        .refresh()
        .map_err(|status| format!("the thread list could not be read ({status:#x})"))?;
    let Some(process) = scanner.entries().find(|e| e.pid() == pid) else {
        return Ok((Vec::new(), 0));
    };
    let (mut tids, mut idle) = (Vec::new(), 0);
    for t in process.threads() {
        let tid = t.info.ClientId.UniqueThread as u32;
        let waiting = t.info.ThreadState == crate::ntapi::ThreadStateWaiting
            && !IDLE_WAITS.contains(&t.info.WaitReason);
        if waiting || Some(tid) == first {
            tids.push(tid);
        } else {
            idle += 1;
        }
    }
    Ok((tids, idle))
}

fn link(node: &WAITCHAIN_NODE_INFO) -> Link {
    let status = Status::from(node.ObjectStatus);
    if node.ObjectType == WctThreadType {
        let t = unsafe { node.Anonymous.ThreadObject };
        Link::Thread {
            pid: t.ProcessId,
            tid: t.ThreadId,
            status,
            context_switches: t.ContextSwitches,
        }
    } else {
        let o = unsafe { node.Anonymous.LockObject };
        Link::Object {
            wait: Wait::from(node.ObjectType),
            name: from_wide(&o.ObjectName),
            status,
        }
    }
}

/// One thread's chain may take this long; then it is skipped. A chain can
/// lead into a process that does not answer, and Windows has no timeout.
const THREAD_TIMEOUT: Duration = Duration::from_millis(2500);
/// The whole analysis; threads not reached by then are skipped.
const BUDGET: Duration = Duration::from_secs(20);
/// Windows spends about 50 ms on each thread, nearly all of it waiting, so
/// several sessions side by side divide the time: 450 threads take about
/// three seconds instead of more than twenty. The first session must have
/// answered once before the others open: sessions opened side by side in a
/// process that has never used one break, and can take the process down.
const WORKERS: usize = 4;

/// What the analysis threads report.
enum Note {
    Started {
        worker: usize,
        tid: u32,
    },
    /// `chain` is None when Windows could not analyse the thread.
    Done {
        worker: usize,
        tid: u32,
        chain: Option<Chain>,
    },
    /// No session could be opened.
    Failed(String),
}

/// Takes threads from `queue` until it is empty, or until it is retired:
/// a chain that hangs inside Windows holds up only this thread, and its
/// answer, when it comes, is no longer wanted.
fn analyse(
    worker: usize,
    flags: u32,
    queue: crossbeam_channel::Receiver<u32>,
    retired: Arc<AtomicBool>,
    tx: crossbeam_channel::Sender<Note>,
) {
    // Below the programs in use: what costs time here is CPU.
    crate::hardening::set_current_thread_priority(crate::hardening::ThreadPriority::BelowNormal);
    // The COM callbacks run on this thread.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let session =
        unsafe { OpenThreadWaitChainSession(OPEN_THREAD_WAIT_CHAIN_SESSION_FLAGS(0), None) };
    if session.is_null() {
        let _ = tx.send(Note::Failed(windows::core::Error::from_thread().message()));
        return;
    }
    while !retired.load(Ordering::Relaxed)
        && let Ok(tid) = queue.try_recv()
    {
        if tx.send(Note::Started { worker, tid }).is_err() {
            break;
        }
        let mut nodes = [WAITCHAIN_NODE_INFO::default(); WCT_MAX_NODE_COUNT as usize];
        let mut count = WCT_MAX_NODE_COUNT;
        let mut cycle = BOOL(0);
        let ok = unsafe {
            GetThreadWaitChain(
                session,
                None,
                WAIT_CHAIN_THREAD_OPTIONS(flags),
                tid,
                &mut count,
                nodes.as_mut_ptr(),
                &mut cycle,
            )
        };
        let chain = ok.is_ok().then(|| Chain {
            links: nodes[..(count as usize).min(nodes.len())]
                .iter()
                .map(link)
                .collect(),
            cycle: cycle.as_bool(),
        });
        if retired.load(Ordering::Relaxed) || tx.send(Note::Done { worker, tid, chain }).is_err() {
            break;
        }
    }
    unsafe { CloseThreadWaitChainSession(session) };
}

/// What the threads of `key` wait for. `first` (a hung window's thread)
/// is analysed and listed first. `keep_going` hears how many threads are
/// done of how many, several times a second; when it returns false the
/// analysis stops with an error nobody needs to show.
pub fn wait_chains(
    key: ProcKey,
    first: Option<u32>,
    keep_going: &mut dyn FnMut(usize, usize) -> bool,
) -> Result<WaitReport, String> {
    let started = Instant::now();
    let com = com_callbacks();
    let (mut tids, idle) = threads_of(key.pid, first)?;
    if tids.is_empty() && idle == 0 {
        return Err("the process has no threads (it has exited)".into());
    }
    if let Some(first) = first
        && let Some(i) = tids.iter().position(|&t| t == first)
    {
        tids[..=i].rotate_right(1);
    }
    let mut flags = WCT_OUT_OF_PROC_FLAG.0 | WCT_OUT_OF_PROC_CS_FLAG.0 | WCT_NETWORK_IO_FLAG;
    if com {
        flags |= WCT_OUT_OF_PROC_COM_FLAG.0;
    }
    let total = tids.len();
    let (work, queue) = crossbeam_channel::unbounded();
    for &tid in &tids {
        let _ = work.send(tid);
    }
    drop(work);
    let (tx, rx) = crossbeam_channel::unbounded();
    struct Worker {
        retired: Arc<AtomicBool>,
        /// The thread it is on, since when.
        on: Option<(u32, Instant)>,
    }
    impl Worker {
        fn retired(&self) -> bool {
            self.retired.load(Ordering::Relaxed)
        }
        fn retire(&mut self) {
            self.retired.store(true, Ordering::Relaxed);
            self.on = None;
        }
    }
    let mut workers: Vec<Worker> = Vec::new();
    let spawn = |workers: &mut Vec<Worker>| {
        let (worker, retired) = (workers.len(), Arc::new(AtomicBool::new(false)));
        let (queue, tx, flag) = (queue.clone(), tx.clone(), retired.clone());
        std::thread::Builder::new()
            .name("anabolic-wait-chain".into())
            .spawn(move || analyse(worker, flags, queue, flag, tx))
            .map(|_| workers.push(Worker { retired, on: None }))
            .map_err(|e| e.to_string())
    };
    if total > 0 {
        spawn(&mut workers)?;
    }
    let mut side_by_side = false;
    let retire_all = |workers: &mut [Worker]| workers.iter_mut().for_each(Worker::retire);

    let mut chains = Vec::new();
    let (mut done, mut failed, mut skipped) = (0, 0, 0);
    while done < total {
        if !keep_going(done, total) {
            retire_all(&mut workers);
            return Err("stopped".into());
        }
        if started.elapsed() > BUDGET {
            skipped += total - done;
            break;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Note::Started { worker, tid }) => {
                if !workers[worker].retired() {
                    workers[worker].on = Some((tid, Instant::now()));
                }
            }
            Ok(Note::Done { worker, tid, chain }) => {
                if workers[worker].retired() {
                    continue;
                }
                workers[worker].on = None;
                done += 1;
                if !side_by_side {
                    side_by_side = true;
                    for _ in 1..WORKERS.min(total - done + 1) {
                        spawn(&mut workers)?;
                    }
                }
                let Some(chain) = chain else {
                    failed += 1;
                    continue;
                };
                let blocked = chain.links.len() > 1
                    && matches!(
                        chain.links.first(),
                        Some(Link::Thread {
                            status: Status::Blocked,
                            ..
                        })
                    );
                if blocked || Some(tid) == first {
                    chains.push(chain);
                }
            }
            Ok(Note::Failed(e)) => {
                retire_all(&mut workers);
                return Err(e);
            }
            Err(_) => {
                // A worker stuck on one thread: leave it to its fate, and
                // let a new one take its place in the queue.
                let stuck: Vec<usize> = (0..workers.len())
                    .filter(|&i| {
                        !workers[i].retired()
                            && workers[i]
                                .on
                                .is_some_and(|(_, since)| since.elapsed() > THREAD_TIMEOUT)
                    })
                    .collect();
                for i in stuck {
                    workers[i].retire();
                    done += 1;
                    skipped += 1;
                    if !queue.is_empty() {
                        spawn(&mut workers)?;
                    }
                }
            }
        }
    }
    retire_all(&mut workers);
    // Deadlocks, then the thread asked about, then the longest chains.
    let first_tid = |c: &Chain| match c.links.first() {
        Some(Link::Thread { tid, .. }) => Some(*tid),
        _ => None,
    };
    chains.sort_by_key(|c| {
        (
            !c.cycle,
            first.is_none() || first_tid(c) != first,
            std::cmp::Reverse(c.links.len()),
        )
    });
    Ok(WaitReport {
        key,
        chains,
        threads: total + idle,
        idle,
        failed,
        skipped,
        took: started.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Our own threads: one waits for another to finish, so its chain
    /// is "blocked on a thread".
    #[test]
    fn finds_a_thread_waiting_for_another() {
        let sleeper = std::thread::spawn(|| std::thread::sleep(Duration::from_secs(3)));
        let waiter = std::thread::spawn(move || sleeper.join().unwrap());
        std::thread::sleep(Duration::from_millis(300));
        let report = wait_chains(
            ProcKey {
                pid: std::process::id(),
                create_time: 0,
            },
            None,
            &mut |_, _| true,
        )
        .expect("wait chains of our own process");
        assert!(report.threads >= 3);
        let waits_on_thread = report.chains.iter().any(|c| {
            c.links.iter().any(|l| {
                matches!(
                    l,
                    Link::Object {
                        wait: Wait::Thread,
                        ..
                    }
                )
            })
        });
        assert!(waits_on_thread, "{:#?}", report.chains);
        waiter.join().unwrap();
    }

    #[test]
    fn hung_window_scan_runs() {
        // Nothing to assert about the desktop; it must not fail or hang.
        let _ = hung_windows();
    }
}
