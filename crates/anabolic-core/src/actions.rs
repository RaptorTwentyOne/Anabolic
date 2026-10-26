// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Process control. Runs on its own thread: ending a process wedged in the
//! kernel, or a slow call into the desktop, must never freeze the UI.

use crate::model::ProcKey;
use crossbeam_channel::{Receiver, Sender, unbounded};
use std::sync::Arc;
use std::thread;

#[cfg_attr(windows, path = "windows/actions.rs")]
#[cfg_attr(target_os = "linux", path = "linux/actions.rs")]
mod imp;
pub use imp::*;

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
    /// Start Anabolic at sign-in (`Some(true)`), or no longer; `None` only
    /// asks whether it does.
    SignIn(Option<bool>),
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
    /// Start something the way the Run box does: a program with its
    /// arguments, a document, a folder or a web address. `elevated` asks
    /// for administrator rights; without it the program runs without them,
    /// from an elevated Anabolic too.
    Run {
        command: String,
        elevated: bool,
    },
    /// Pick a program file for Run new task, in the system's Open dialog.
    BrowseProgram,
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
    /// Whether Anabolic starts at sign-in, and why setting it failed.
    SignIn {
        state: crate::signin::SignIn,
        error: Option<String>,
    },
    /// The file picked for Run new task; None when the dialog was cancelled.
    Browsed(Option<String>),
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
                imp::thread_init();
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
                    // The Open dialog waits on the user: off this thread,
                    // so the actions behind it are not held up.
                    if matches!(action, Action::BrowseProgram) {
                        let (out_tx, notify) = (out_tx.clone(), notify.clone());
                        let _ = thread::Builder::new().name("anabolic-browse".into()).spawn(
                            move || {
                                imp::thread_init();
                                if let Some(outcome) = perform(action) {
                                    let _ = out_tx.send(outcome);
                                }
                                notify();
                            },
                        );
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

/// A Run box line split into what to start and its arguments: a quoted
/// first part, else the longest start of the line that `exists` (a path
/// with spaces needs no quotes), else the first word.
pub fn split_command(line: &str, exists: impl Fn(&str) -> bool) -> (String, String) {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix('"') {
        return match rest.split_once('"') {
            Some((program, args)) => (program.to_string(), args.trim().to_string()),
            None => (rest.to_string(), String::new()),
        };
    }
    let mut ends: Vec<usize> = line
        .char_indices()
        .filter(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .collect();
    ends.push(line.len());
    for &end in ends.iter().rev() {
        let program = &line[..end];
        if exists(program) {
            return (program.to_string(), line[end..].trim().to_string());
        }
    }
    let end = ends[0];
    (line[..end].to_string(), line[end..].trim().to_string())
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

#[cfg(test)]
mod tests {
    use super::split_command;

    #[test]
    fn splits_a_run_line_as_the_run_box_does() {
        let none = |_: &str| false;
        assert_eq!(
            split_command("  cmd /k dir  ", none),
            ("cmd".into(), "/k dir".into())
        );
        assert_eq!(
            split_command(r#""C:\Program Files\App\app.exe" --new"#, none),
            (r"C:\Program Files\App\app.exe".into(), "--new".into())
        );
        let app = |p: &str| p == r"C:\Program Files\App\app.exe";
        assert_eq!(
            split_command(r"C:\Program Files\App\app.exe --new x", app),
            (r"C:\Program Files\App\app.exe".into(), "--new x".into())
        );
        assert_eq!(
            split_command("https://example.com", none),
            ("https://example.com".into(), String::new())
        );
    }
}
