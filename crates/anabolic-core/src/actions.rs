//! Process control. Runs on its own thread: a TerminateProcess against a
//! process wedged in the kernel, or a slow shell call, must never freeze the UI.

use core::ffi::c_void;
use core::mem::size_of;
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use windows::Win32::Foundation::{DUPLICATE_CLOSE_SOURCE, DuplicateHandle, HANDLE, HWND};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
};
use windows::Win32::System::Threading::{
    ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, GetProcessAffinityMask,
    HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, PROCESS_CREATION_FLAGS,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE, ProcessPowerThrottling, REALTIME_PRIORITY_CLASS,
    SetPriorityClass, SetProcessAffinityMask, SetProcessInformation, TerminateProcess,
};
use windows::Win32::UI::Shell::{
    ILCreateFromPathW, ILFree, SHOP_FILEPATH, SHObjectProperties, SHOpenFolderAndSelectItems,
    ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

use crate::model::ProcKey;
use crate::ntapi;
use crate::util::{error_text, open_process, wide};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Priority {
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
    Realtime,
}

impl Priority {
    pub const ALL: [Priority; 6] = [
        Priority::Realtime,
        Priority::High,
        Priority::AboveNormal,
        Priority::Normal,
        Priority::BelowNormal,
        Priority::Idle,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Priority::Idle => "Idle",
            Priority::BelowNormal => "Below normal",
            Priority::Normal => "Normal",
            Priority::AboveNormal => "Above normal",
            Priority::High => "High",
            Priority::Realtime => "Realtime",
        }
    }

    /// Maps a process' base priority back to its priority class.
    pub fn from_base(base: i32) -> Option<Self> {
        Some(match base {
            4 => Priority::Idle,
            6 => Priority::BelowNormal,
            8 => Priority::Normal,
            10 => Priority::AboveNormal,
            13 => Priority::High,
            24 => Priority::Realtime,
            _ => return None,
        })
    }

    fn class(self) -> PROCESS_CREATION_FLAGS {
        match self {
            Priority::Idle => IDLE_PRIORITY_CLASS,
            Priority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
            Priority::Normal => NORMAL_PRIORITY_CLASS,
            Priority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
            Priority::High => HIGH_PRIORITY_CLASS,
            Priority::Realtime => REALTIME_PRIORITY_CLASS,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Target {
    pub key: ProcKey,
    pub name: Arc<str>,
}

impl Target {
    fn label(&self) -> String {
        format!("{} ({})", self.name, self.key.pid)
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    /// Terminated in order; pass children before parents for a tree kill.
    Terminate(Vec<Target>),
    Suspend(Target),
    Resume(Target),
    SetPriority(Target, Priority),
    SetEfficiencyMode(Target, bool),
    QueryAffinity(Target),
    SetAffinity(Target, u64),
    OpenFileLocation(String),
    FileProperties(String),
    SearchOnline(String),
    /// Open a web page in the default browser.
    OpenUrl(String),
    /// Suspend, resume or end one thread, or set its priority.
    Thread {
        pid: u32,
        tid: u32,
        op: crate::threads::ThreadOp,
    },
    /// Closes handles inside another process at once; `what` names what
    /// they point at, for the report.
    CloseHandles {
        pid: u32,
        process: Arc<str>,
        handles: Vec<u64>,
        what: String,
    },
    /// Closes a handle inside another process (what "unlock file" tools do).
    CloseHandle {
        pid: u32,
        process: Arc<str>,
        handle: u64,
        name: String,
    },
    /// Put text on the clipboard, which another program may be holding.
    Copy(String),
    /// Start this program again elevated. Waits on the UAC prompt.
    RelaunchElevated,
    /// Done on the user's standing orders (a rule), not a click: only a
    /// failure is reported.
    Quiet(Box<Action>),
    /// Nothing is reported at all: game mode checks the result itself on
    /// the next refresh (a protected process refuses, and it should not
    /// say so every time).
    Silent(Box<Action>),
    /// Block a program file from the network with Windows Firewall rules,
    /// or remove them.
    Firewall {
        path: String,
        block: bool,
    },
    /// Which of these blocked programs still have their rules.
    QueryFirewall(Vec<String>),
    /// Drop one TCP connection (IPv4) of a process; it is not opened.
    CloseConnection {
        pid: u32,
        process: Arc<str>,
        remote: std::net::Ipv4Addr,
        remote_port: u16,
        local_port: u16,
    },
    /// Game mode: switch to the High performance power plan (`true`), or
    /// back to the plan it replaced (`false`, with that plan's id).
    GamePower {
        on: bool,
        previous: Option<String>,
    },
    /// Write a memory dump into the folder, on a thread of its own: a full
    /// dump of a big process takes a while.
    Dump {
        target: Target,
        kind: crate::dump::DumpKind,
        dir: std::path::PathBuf,
        /// lsass too (the user's setting).
        secrets: bool,
    },
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Info(String),
    Error(String),
    /// Some processes could not be ended: `keys` are those, so their
    /// details can say why.
    Refused {
        keys: Vec<ProcKey>,
        message: String,
    },
    /// The elevated instance is starting: this one should close.
    Relaunched,
    Affinity {
        target: Target,
        process_mask: u64,
        system_mask: u64,
    },
    Dumped {
        target: Target,
        kind: crate::dump::DumpKind,
        report: crate::dump::DumpReport,
    },
    /// Game mode switched the power plan: the one it replaced (id, name),
    /// None when a fast plan was already on.
    PowerForGame(Option<(String, String)>),
    /// Game mode put this plan back (None: someone changed it meanwhile).
    PowerRestored(Option<String>),
    /// A program was blocked from the network (or allowed again).
    Firewall {
        path: String,
        blocked: bool,
    },
    /// Of the programs Anabolic blocked, those whose rules are still there.
    StillBlocked(Vec<String>),
}

pub struct ActionRunner {
    tx: Sender<Action>,
}

impl ActionRunner {
    pub fn start(notify: impl Fn() + Send + Sync + 'static) -> (Self, Receiver<Outcome>) {
        let (tx, rx) = unbounded::<Action>();
        let (out_tx, out_rx) = unbounded();
        let notify = Arc::new(notify);
        thread::Builder::new()
            .name("anabolic-actions".into())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
                }
                for action in rx {
                    if let Action::Dump {
                        target,
                        kind,
                        dir,
                        secrets,
                    } = action
                    {
                        let (out_tx, notify) = (out_tx.clone(), notify.clone());
                        let _ =
                            thread::Builder::new()
                                .name("anabolic-dump".into())
                                .spawn(move || {
                                    let outcome = match crate::dump::write(
                                        target.key,
                                        &target.name,
                                        kind,
                                        &dir,
                                        secrets,
                                    ) {
                                        Ok(report) => Outcome::Dumped {
                                            target,
                                            kind,
                                            report,
                                        },
                                        Err(e) => Outcome::Error(e),
                                    };
                                    let _ = out_tx.send(outcome);
                                    notify();
                                });
                        continue;
                    }
                    if let Some(outcome) = perform(action)
                        && out_tx.send(outcome).is_err()
                    {
                        return;
                    }
                    notify();
                }
            })
            .expect("failed to spawn the action thread");
        (Self { tx }, out_rx)
    }

    pub fn run(&self, action: Action) {
        let _ = self.tx.send(action);
    }
}

/// Carries out an action on the calling thread (the action thread, or the
/// MCP server's): the outcome, or None for one that has nothing to report.
pub fn perform(action: Action) -> Option<Outcome> {
    let result = match action {
        Action::Quiet(action) => {
            return perform(*action).filter(|o| matches!(o, Outcome::Error(_)));
        }
        Action::Silent(action) => {
            perform(*action);
            return None;
        }
        Action::Firewall { path, block } => {
            let file = std::path::Path::new(&path)
                .file_name()
                .map_or(path.clone(), |f| f.to_string_lossy().into_owned());
            let done = if block {
                crate::firewall::block(&path)
            } else {
                crate::firewall::unblock(&path)
            };
            return Some(match done {
                Ok(()) => Outcome::Firewall {
                    path,
                    blocked: block,
                },
                Err(e) if block => Outcome::Error(format!("Could not block {file}: {e}")),
                Err(e) => Outcome::Error(format!("Could not allow {file} again: {e}")),
            });
        }
        Action::CloseConnection {
            pid,
            process,
            remote,
            remote_port,
            local_port,
        } => crate::sockets::close_tcp(pid, remote, remote_port, local_port)
            .map(|_| format!("Closed {process}'s connection to {remote}:{remote_port}"))
            .map_err(|e| {
                format!("Could not close {process}'s connection to {remote}:{remote_port}: {e}")
            }),
        Action::QueryFirewall(paths) => {
            return Some(Outcome::StillBlocked(crate::firewall::still_blocked(
                &paths,
            )));
        }
        Action::GamePower { on: true, .. } => {
            return Some(match crate::power::for_game() {
                Ok(previous) => {
                    Outcome::PowerForGame(previous.map(|(id, name)| (format!("{id:?}"), name)))
                }
                Err(e) => Outcome::Error(format!("Game mode could not switch the power plan: {e}")),
            });
        }
        Action::GamePower {
            on: false,
            previous,
        } => {
            let previous = previous.as_deref().and_then(crate::power::parse)?;
            return Some(match crate::power::restore(&previous) {
                Ok(name) => Outcome::PowerRestored(name),
                Err(e) => Outcome::Error(format!("Could not put the power plan back: {e}")),
            });
        }
        Action::Copy(text) => arboard::Clipboard::new()
            .and_then(|mut c| c.set_text(text))
            .map(|_| String::new())
            .map_err(|e| format!("Could not copy: {e}")),
        Action::RelaunchElevated => {
            return Some(match relaunch_elevated() {
                Ok(()) => Outcome::Relaunched,
                Err(e) => Outcome::Error(e),
            });
        }
        Action::Terminate(targets) => return Some(terminate(&targets)),
        // Run on their own thread by the runner.
        Action::Dump { .. } => return None,
        Action::Suspend(t) => nt_call(&t, PROCESS_SUSPEND_RESUME, ntapi::NtSuspendProcess)
            .map(|_| format!("Suspended {}", t.label())),
        Action::Resume(t) => nt_call(&t, PROCESS_SUSPEND_RESUME, ntapi::NtResumeProcess)
            .map(|_| format!("Resumed {}", t.label())),
        Action::SetPriority(t, priority) => open_process(t.key, PROCESS_SET_INFORMATION)
            .and_then(|h| unsafe { SetPriorityClass(h.raw(), priority.class()) })
            .map(|_| format!("{} priority set to {}", t.label(), priority.label()))
            .map_err(|e| {
                format!(
                    "Could not change priority of {}: {}",
                    t.label(),
                    error_text(&e)
                )
            }),
        Action::SetEfficiencyMode(t, on) => set_efficiency_mode(&t, on),
        Action::QueryAffinity(t) => {
            let mut process_mask = 0usize;
            let mut system_mask = 0usize;
            return Some(
                match open_process(t.key, PROCESS_QUERY_LIMITED_INFORMATION).and_then(|h| unsafe {
                    GetProcessAffinityMask(h.raw(), &mut process_mask, &mut system_mask)
                }) {
                    Ok(()) => Outcome::Affinity {
                        target: t,
                        process_mask: process_mask as u64,
                        system_mask: system_mask as u64,
                    },
                    Err(e) => Outcome::Error(format!(
                        "Could not read affinity of {}: {}",
                        t.label(),
                        error_text(&e)
                    )),
                },
            );
        }
        Action::SetAffinity(t, mask) => open_process(t.key, PROCESS_SET_INFORMATION)
            .and_then(|h| unsafe { SetProcessAffinityMask(h.raw(), mask as usize) })
            .map(|_| format!("{} affinity set to {} CPU(s)", t.label(), mask.count_ones()))
            .map_err(|e| {
                format!(
                    "Could not set affinity of {}: {}",
                    t.label(),
                    error_text(&e)
                )
            }),
        Action::OpenFileLocation(path) => open_file_location(&path).map(|_| String::new()),
        Action::FileProperties(path) => {
            let p = wide(&path);
            let ok = unsafe {
                SHObjectProperties(
                    None::<HWND>,
                    SHOP_FILEPATH,
                    PCWSTR(p.as_ptr()),
                    PCWSTR::null(),
                )
            };
            if ok.as_bool() {
                return None;
            } else {
                Err(format!("Could not show properties of {path}"))
            }
        }
        Action::Thread { pid, tid, op } => crate::threads::apply(pid, tid, op),
        Action::CloseHandles {
            pid,
            process,
            handles,
            what,
        } => {
            let failed: Vec<String> = handles
                .iter()
                .filter_map(|&h| {
                    close_remote_handle(pid, h)
                        .err()
                        .map(|e| format!("0x{h:X}: {e}"))
                })
                .collect();
            if failed.is_empty() {
                Ok(format!(
                    "Closed {} handles to {what} in {process} ({pid})",
                    handles.len()
                ))
            } else {
                Err(format!(
                    "Could not close {} of {} handles to {what} in {process} ({pid}): {}",
                    failed.len(),
                    handles.len(),
                    failed.join("; ")
                ))
            }
        }
        Action::CloseHandle {
            pid,
            process,
            handle,
            name,
        } => close_remote_handle(pid, handle)
            .map(|_| format!("Closed handle 0x{handle:X} ({name}) in {process} ({pid})"))
            .map_err(|e| format!("Could not close handle 0x{handle:X} in {process} ({pid}): {e}")),
        Action::OpenUrl(url) => {
            shell_open(&url);
            return None;
        }
        Action::SearchOnline(query) => {
            shell_open(&format!(
                "https://www.google.com/search?q={}",
                url_encode(&query)
            ));
            return None;
        }
    };
    Some(match result {
        Ok(msg) if msg.is_empty() => return None,
        Ok(msg) => Outcome::Info(msg),
        Err(msg) => Outcome::Error(msg),
    })
}

fn terminate(targets: &[Target]) -> Outcome {
    let mut failures = Vec::new();
    let mut refused = Vec::new();
    let mut done = 0;
    for t in targets {
        match open_process(t.key, PROCESS_TERMINATE)
            .and_then(|h| unsafe { TerminateProcess(h.raw(), 1) })
        {
            Ok(()) => done += 1,
            Err(e) => {
                // "Access is denied" is what Windows also answers for a
                // process that is ending already; say which it is.
                let ending = crate::ending::is_ending(t.key.pid);
                let why = if ending {
                    "it is ending already and Windows cannot finish".to_string()
                } else {
                    error_text(&e)
                };
                failures.push(format!("{}: {why}", t.label()));
                refused.push(t.key);
            }
        }
    }
    match (done, failures.is_empty()) {
        (_, false) => Outcome::Refused {
            keys: refused,
            message: format!(
                "Could not terminate {}{}. The details say why.",
                failures.join("; "),
                if done > 0 {
                    format!(" (ended {done} others)")
                } else {
                    String::new()
                }
            ),
        },
        (1, true) => Outcome::Info(format!("Terminated {}", targets[0].label())),
        (n, true) => Outcome::Info(format!("Terminated {n} processes")),
    }
}

fn nt_call(
    t: &Target,
    access: windows::Win32::System::Threading::PROCESS_ACCESS_RIGHTS,
    call: unsafe extern "system" fn(windows::Win32::Foundation::HANDLE) -> ntapi::NTSTATUS,
) -> Result<(), String> {
    let h =
        open_process(t.key, access).map_err(|e| format!("{}: {}", t.label(), error_text(&e)))?;
    let status = unsafe { call(h.raw()) };
    if ntapi::nt_success(status) {
        Ok(())
    } else {
        Err(format!("{}: {}", t.label(), ntapi::status_message(status)))
    }
}

/// The same thing Task Manager's "Efficiency mode" does: EcoQoS + idle priority.
fn set_efficiency_mode(t: &Target, on: bool) -> Result<String, String> {
    let h = open_process(t.key, PROCESS_SET_INFORMATION)
        .map_err(|e| format!("{}: {}", t.label(), error_text(&e)))?;
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: if on {
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED
        } else {
            0
        },
    };
    unsafe {
        SetProcessInformation(
            h.raw(),
            ProcessPowerThrottling,
            (&raw const state).cast::<c_void>(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .and_then(|_| unsafe {
        SetPriorityClass(
            h.raw(),
            if on {
                IDLE_PRIORITY_CLASS
            } else {
                NORMAL_PRIORITY_CLASS
            },
        )
    })
    .map(|_| {
        format!(
            "Efficiency mode {} for {}",
            if on { "on" } else { "off" },
            t.label()
        )
    })
    .map_err(|e| format!("{}: {}", t.label(), error_text(&e)))
}

fn open_file_location(path: &str) -> Result<(), String> {
    let p = wide(path);
    unsafe {
        let pidl = ILCreateFromPathW(PCWSTR(p.as_ptr()));
        if pidl.is_null() {
            return Err(format!("Path not found: {path}"));
        }
        let result = SHOpenFolderAndSelectItems(pidl, None, 0);
        ILFree(Some(pidl));
        result.map_err(|e| error_text(&e))
    }
}

fn shell_open(target: &str) {
    let t = wide(target);
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(t.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// Starts a new elevated instance of the current executable (UAC prompt).
/// It waits for this one to exit before taking over as the session's
/// Anabolic (`--after`), or it would find us still running and hand over.
fn relaunch_elevated() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = wide(&exe.to_string_lossy());
    let args = wide(&format!("--after {}", std::process::id()));
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            PCWSTR(exe.as_ptr()),
            PCWSTR(args.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as usize > 32 {
        Ok(())
    } else {
        Err("elevation was cancelled or failed".into())
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn close_remote_handle(pid: u32, handle: u64) -> Result<(), String> {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_DUP_HANDLE};
    let target = crate::util::OwnedHandle(
        unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, pid) }.map_err(|e| error_text(&e))?,
    );
    unsafe {
        DuplicateHandle(
            target.raw(),
            HANDLE(handle as *mut core::ffi::c_void),
            HANDLE::default(),
            std::ptr::null_mut(),
            0,
            false,
            DUPLICATE_CLOSE_SOURCE,
        )
    }
    .map_err(|e| error_text(&e))
}
