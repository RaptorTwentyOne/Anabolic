// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What starts with the session or the machine, where it is configured, and
//! turning it on and off the way the system's own settings do. Nothing is
//! deleted.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};

#[cfg_attr(windows, path = "windows/startup.rs")]
#[cfg_attr(target_os = "linux", path = "linux/startup.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Debug)]
pub struct StartupItem {
    /// The value name, the file name in the folder, or the task's name.
    pub name: String,
    pub source: Source,
    /// The task's full path (`\Folder\Name`); the file's for a folder item.
    pub location: String,
    /// What runs: a command line.
    pub command: String,
    /// The program that command starts, environment variables expanded.
    pub program: Option<String>,
    pub enabled: bool,
    /// When it was turned off, as a FILETIME (Task Manager records it).
    pub disabled_at: Option<i64>,
    pub publisher: Option<String>,
    pub description: Option<String>,
    /// Part of the system: a task of Windows' own (under \Microsoft\), or
    /// on Linux an entry the system's packages put in place.
    pub system: bool,
    /// The program is not there any more.
    pub missing: bool,
}

#[derive(Debug, Default)]
pub struct StartupList {
    pub items: Vec<StartupItem>,
    /// Sources that could not be read, and why.
    pub errors: Vec<String>,
    pub took: Duration,
}

#[derive(Debug)]
pub enum StartupEvent {
    List(Arc<StartupList>),
    Done(Result<String, String>),
}

enum Request {
    Refresh,
    Set(Box<StartupItem>, bool),
}

/// Reads and changes the startup entries on a thread of its own (COM, the
/// registry and every program's version resource are slow to reach).
pub struct StartupWatch {
    tx: Sender<Request>,
}

impl StartupWatch {
    pub fn start(notify: impl Fn() + Send + 'static) -> (Self, Receiver<StartupEvent>) {
        let (tx, rx) = unbounded::<Request>();
        let (events, out) = unbounded();
        thread::Builder::new()
            .name("anabolic-startup".into())
            .spawn(move || {
                imp::thread_init();
                crate::hardening::set_current_thread_priority(
                    crate::hardening::ThreadPriority::BelowNormal,
                );
                while let Ok(request) = rx.recv() {
                    let mut refresh = false;
                    for request in std::iter::once(request).chain(rx.try_iter()) {
                        match request {
                            Request::Refresh => refresh = true,
                            Request::Set(item, on) => {
                                let done = set_enabled(&item, on);
                                if events.send(StartupEvent::Done(done)).is_err() {
                                    return;
                                }
                                refresh = true;
                            }
                        }
                    }
                    if refresh && events.send(StartupEvent::List(Arc::new(list()))).is_err() {
                        return;
                    }
                    notify();
                }
            })
            .expect("failed to spawn the startup thread");
        (Self { tx }, out)
    }

    pub fn refresh(&self) {
        let _ = self.tx.send(Request::Refresh);
    }

    pub fn set_enabled(&self, item: StartupItem, on: bool) {
        let _ = self.tx.send(Request::Set(Box::new(item), on));
    }
}
