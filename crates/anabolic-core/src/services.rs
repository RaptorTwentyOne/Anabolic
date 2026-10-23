// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Services: each one's state, start type and host process, and starting,
//! stopping and reconfiguring them. Windows' Service Control Manager or
//! systemd answers, and a stop can take half a minute, so it all runs on
//! threads of its own, never the UI's.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};

#[cfg_attr(windows, path = "windows/services.rs")]
#[cfg_attr(target_os = "linux", path = "linux/services.rs")]
mod imp;
pub use imp::exit_text;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Stopped,
    Starting,
    Stopping,
    Running,
    Continuing,
    Pausing,
    Paused,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Stopped => "Stopped",
            State::Starting => "Starting",
            State::Stopping => "Stopping",
            State::Running => "Running",
            State::Continuing => "Resuming",
            State::Pausing => "Pausing",
            State::Paused => "Paused",
        }
    }

    /// Between two states: a control is under way.
    pub fn pending(self) -> bool {
        matches!(
            self,
            State::Starting | State::Stopping | State::Continuing | State::Pausing
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StartType {
    /// Drivers only: loaded by the boot loader or the kernel.
    Boot,
    /// Windows: drivers the kernel loads. systemd: a static unit, which
    /// has no switch and starts when another unit needs it.
    System,
    Automatic,
    /// Started a couple of minutes after the others, to free up sign-in.
    AutomaticDelayed,
    Manual,
    Disabled,
}

impl StartType {
    /// What a service can be set to.
    #[cfg(windows)]
    pub const CHOICES: [StartType; 4] = [
        StartType::Automatic,
        StartType::AutomaticDelayed,
        StartType::Manual,
        StartType::Disabled,
    ];

    /// What a service can be set to: systemd has no delayed start.
    #[cfg(target_os = "linux")]
    pub const CHOICES: [StartType; 3] =
        [StartType::Automatic, StartType::Manual, StartType::Disabled];

    #[cfg(windows)]
    pub fn label(self) -> &'static str {
        match self {
            StartType::Boot => "Boot",
            StartType::System => "System",
            StartType::Automatic => "Automatic",
            StartType::AutomaticDelayed => "Automatic (delayed)",
            StartType::Manual => "Manual",
            StartType::Disabled => "Disabled",
        }
    }

    /// systemd's own words: enabled units start at boot (or sign-in),
    /// disabled ones when asked or needed, masked ones never, static ones
    /// when another unit needs them.
    #[cfg(target_os = "linux")]
    pub fn label(self) -> &'static str {
        match self {
            StartType::Boot => "Boot",
            StartType::System => "Static",
            StartType::Automatic => "Enabled",
            StartType::AutomaticDelayed => "Enabled (delayed)",
            StartType::Manual => "Disabled",
            StartType::Disabled => "Masked",
        }
    }
}

/// What rarely changes about a service; read once and kept.
#[derive(Clone, Debug)]
pub struct Config {
    pub start: StartType,
    /// Also started (or stopped) by an event: a device arriving, an IP
    /// address, a group policy change.
    pub triggered: bool,
    /// The command line the service runs; svchost's names its group.
    pub binary: String,
    /// The account it logs on as.
    pub account: String,
    pub description: String,
    /// Services (or "+group"s) that must run first.
    pub dependencies: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Service {
    pub name: Arc<str>,
    pub display: Arc<str>,
    pub state: State,
    /// The process it runs in, 0 while stopped.
    pub pid: u32,
    /// Its kind can share a process with other services (svchost).
    pub shared: bool,
    /// A per-user template; Windows starts a copy of it for each user, and
    /// the template itself cannot be started.
    pub user_template: bool,
    pub accepts_stop: bool,
    pub accepts_pause: bool,
    /// Why it last stopped, when that was a failure.
    pub exit_code: u32,
    /// None when the configuration could not be read (no rights).
    pub config: Option<Arc<Config>>,
}

impl Service {
    pub fn start_type(&self) -> Option<StartType> {
        self.config.as_ref().map(|c| c.start)
    }
}

#[derive(Debug, Default)]
pub struct ServiceList {
    pub services: Vec<Service>,
    pub error: Option<String>,
    pub took: Duration,
}

impl ServiceList {
    /// The services each process runs.
    pub fn by_pid(&self) -> HashMap<u32, Vec<usize>> {
        let mut map: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, s) in self.services.iter().enumerate() {
            if s.pid != 0 {
                map.entry(s.pid).or_default().push(i);
            }
        }
        map
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ServiceOp {
    Start,
    /// `dependents`: stop the services that depend on it first.
    Stop {
        dependents: bool,
    },
    /// Stop and start again; `dependents` as for Stop, and they are
    /// started again afterwards.
    Restart {
        dependents: bool,
    },
    Pause,
    Continue,
    SetStart(StartType),
}

#[derive(Debug)]
pub enum ServiceEvent {
    List(Arc<ServiceList>),
    /// A control finished. `dependents` names the running services that
    /// keep it from stopping, when that is why it failed.
    Done {
        name: Arc<str>,
        op: ServiceOp,
        result: Result<String, String>,
        dependents: Vec<String>,
    },
}

/// Every service now, read on the calling thread.
pub fn query() -> ServiceList {
    imp::list(&mut HashMap::new())
}

/// Carries out one control on the calling thread: the outcome in words, and
/// the running services that depend on it when that is why it failed.
pub fn control_now(
    name: &str,
    display: &str,
    op: ServiceOp,
) -> (Result<String, String>, Vec<String>) {
    imp::control(name, display, op)
}

enum Request {
    Refresh,
    /// Its configuration changed: read it again on the next refresh.
    Forget(Arc<str>),
}

/// Lists the services on request, on a thread of its own; controls run on
/// short-lived threads beside it, as a stop can wait half a minute.
pub struct ServiceWatch {
    tx: Sender<Request>,
    events: Sender<ServiceEvent>,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl ServiceWatch {
    pub fn start(notify: impl Fn() + Send + Sync + 'static) -> (Self, Receiver<ServiceEvent>) {
        let (tx, rx) = unbounded::<Request>();
        let (events, out) = unbounded();
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(notify);
        {
            let (events, notify) = (events.clone(), notify.clone());
            thread::Builder::new()
                .name("anabolic-services".into())
                .spawn(move || {
                    crate::hardening::set_current_thread_priority(
                        crate::hardening::ThreadPriority::BelowNormal,
                    );
                    let mut configs = HashMap::new();
                    while let Ok(request) = rx.recv() {
                        // Refreshes asked for meanwhile are one refresh.
                        let mut requests = vec![request];
                        requests.extend(rx.try_iter());
                        for r in &requests {
                            if let Request::Forget(name) = r {
                                configs.remove(name);
                            }
                        }
                        let list = imp::list(&mut configs);
                        if events.send(ServiceEvent::List(Arc::new(list))).is_err() {
                            return;
                        }
                        notify();
                    }
                })
                .expect("failed to spawn the services thread");
        }
        (Self { tx, events, notify }, out)
    }

    pub fn refresh(&self) {
        let _ = self.tx.send(Request::Refresh);
    }

    pub fn control(&self, name: Arc<str>, display: Arc<str>, op: ServiceOp) {
        let (tx, events, notify) = (self.tx.clone(), self.events.clone(), self.notify.clone());
        let _ = thread::Builder::new()
            .name("anabolic-service-control".into())
            .spawn(move || {
                let (result, dependents) = imp::control(&name, &display, op);
                if matches!(op, ServiceOp::SetStart(_)) {
                    let _ = tx.send(Request::Forget(name.clone()));
                }
                let _ = events.send(ServiceEvent::Done {
                    name,
                    op,
                    result,
                    dependents,
                });
                let _ = tx.send(Request::Refresh);
                notify();
            });
        // The pending state shows at once.
        self.refresh();
    }
}
