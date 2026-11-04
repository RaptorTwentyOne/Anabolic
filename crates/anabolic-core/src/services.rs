// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Services: each one's state, start type and host process; starting,
//! stopping and reconfiguring them; and creating, changing in full and
//! deleting them. Windows' Service Control Manager or systemd answers, a
//! stop can take half a minute and pkexec waits for a password, so it all
//! runs on threads of its own, never the UI's.

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
    /// A service's full settings, asked for with `ServiceWatch::detail`.
    Detail {
        name: Arc<str>,
        detail: Result<Box<ServiceDetail>, String>,
    },
    /// A service was created, changed or deleted (`ServiceWatch::change`).
    Changed {
        name: Arc<str>,
        outcome: ChangeOutcome,
    },
}

/// What a change came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeOutcome {
    /// What was done, or what went wrong, in words.
    pub result: Result<String, String>,
    /// Something was written, even if not all of it: the service is not as
    /// it was, and asking again for the same change would not do the same.
    pub written: bool,
}

impl ChangeOutcome {
    pub fn done(text: String) -> Self {
        Self {
            result: Ok(text),
            written: true,
        }
    }

    /// Nothing was written.
    pub fn refused(text: String) -> Self {
        Self {
            result: Err(text),
            written: false,
        }
    }

    /// Written in part: the error says what is missing.
    pub fn partly(text: String) -> Self {
        Self {
            result: Err(text),
            written: true,
        }
    }
}

/// Who a service runs as.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Account {
    /// Windows' LocalSystem; on Linux, root.
    System,
    /// Windows' NT AUTHORITY\LocalService: few rights, anonymous on the
    /// network.
    LocalService,
    /// Windows' NT AUTHORITY\NetworkService: few rights, the computer's
    /// own on the network.
    NetworkService,
    /// A user account: `.\name`, `DOMAIN\name` or `name@domain` on Windows,
    /// a user name on Linux.
    User(String),
}

impl Account {
    /// The account in the words the system itself uses.
    pub fn label(&self) -> String {
        match self {
            Account::System if cfg!(windows) => "Local System".into(),
            Account::System => "root".into(),
            Account::LocalService => "Local Service".into(),
            Account::NetworkService => "Network Service".into(),
            Account::User(name) => name.clone(),
        }
    }
}

/// A password to set, never read back; kept out of debug output.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Password(pub String);

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(…)")
    }
}

/// What the service manager does when a service fails.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FailureAction {
    Nothing,
    Restart,
    /// Runs the recovery program (Windows).
    RunProgram,
    /// Restarts the computer (Windows). Shown and kept when a service has
    /// it, never offered.
    Reboot,
}

impl FailureAction {
    pub fn label(self) -> &'static str {
        match self {
            FailureAction::Nothing => "Take no action",
            FailureAction::Restart => "Restart the service",
            FailureAction::RunProgram => "Run a program",
            FailureAction::Reboot => "Restart the computer",
        }
    }
}

/// How a service recovers from a failure.
#[derive(Clone, PartialEq, Debug)]
pub struct Recovery {
    /// What the first, second and later failures do (Windows). systemd
    /// answers every failure the same way: the first.
    pub actions: [FailureAction; 3],
    /// How long after the failure the action comes.
    pub delay: Duration,
    /// Windows: after how many days without a failure the count starts
    /// again; None: never.
    pub reset_days: Option<u32>,
    /// Windows: the command line "Run a program" runs.
    pub program: String,
    /// Windows: also when it stops with an error of its own, not only when
    /// its process ends unexpectedly. systemd: restart it also when it ends
    /// without an error (Restart=always).
    pub also: bool,
}

impl Default for Recovery {
    fn default() -> Self {
        Self {
            actions: [FailureAction::Nothing; 3],
            delay: Duration::from_secs(60),
            reset_days: Some(1),
            program: String::new(),
            also: false,
        }
    }
}

/// How bad Windows takes it when a service fails to start at boot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorControl {
    /// It goes on and logs nothing.
    Ignore,
    /// It logs the error and goes on.
    Normal,
    /// It logs it, and starts again with the last known good settings.
    Severe,
    /// As severe, and fails to start if those fail too. Shown, not offered.
    Critical,
}

impl ErrorControl {
    pub const CHOICES: [ErrorControl; 3] = [
        ErrorControl::Ignore,
        ErrorControl::Normal,
        ErrorControl::Severe,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ErrorControl::Ignore => "Ignore",
            ErrorControl::Normal => "Normal",
            ErrorControl::Severe => "Severe",
            ErrorControl::Critical => "Critical",
        }
    }
}

/// A service's settings: what a new one is created with, or what an
/// existing one is changed to.
#[derive(Clone, PartialEq, Debug)]
pub struct ServiceSpec {
    /// Its key name (Windows), or its unit's name (Linux, `.service` added
    /// when missing). Set once, at creation.
    pub name: String,
    /// The name shown for it; on Linux its Description.
    pub display: String,
    /// What it does (Windows; systemd units have only the one line).
    pub description: String,
    /// The command line it runs.
    pub program: String,
    pub start: StartType,
    pub account: Account,
    /// A user account's password, when it is set or changed.
    pub password: Option<Password>,
    /// The services it needs running first (Windows: "+name" for a group).
    pub dependencies: Vec<String>,
    /// Windows only.
    pub error_control: ErrorControl,
    pub recovery: Recovery,
    /// Linux: one of the user's own units (systemctl --user), which needs
    /// no administrator.
    pub own: bool,
}

impl Default for ServiceSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            display: String::new(),
            description: String::new(),
            program: String::new(),
            start: StartType::Manual,
            account: Account::System,
            password: None,
            dependencies: Vec::new(),
            error_control: ErrorControl::Normal,
            recovery: Recovery::default(),
            own: false,
        }
    }
}

/// A service's settings as they are now, and what may change.
#[derive(Clone, Debug)]
pub struct ServiceDetail {
    pub spec: ServiceSpec,
    /// Why it cannot be deleted, when it cannot.
    pub keep: Option<String>,
    /// Why its program and account cannot be changed, when they cannot.
    pub fixed: Option<String>,
    /// Where its settings live: the registry key, or the unit file.
    pub source: String,
    /// Linux: changes go to a drop-in beside a package's unit file, not
    /// into the file itself.
    pub drop_in: bool,
}

/// A change to the set of services.
#[derive(Clone, Debug)]
pub enum ServiceChange {
    Create(Box<ServiceSpec>),
    /// `before` is the detail it was read with: only what differs from it
    /// is written.
    Configure {
        before: Box<ServiceSpec>,
        after: Box<ServiceSpec>,
    },
    /// `stop`: stop it first when it runs.
    Delete {
        name: Arc<str>,
        display: Arc<str>,
        stop: bool,
    },
}

impl ServiceChange {
    pub fn name(&self) -> Arc<str> {
        match self {
            ServiceChange::Create(spec) => imp::canonical(&spec.name, spec.own).into(),
            ServiceChange::Configure { before, .. } => before.name.as_str().into(),
            ServiceChange::Delete { name, .. } => name.clone(),
        }
    }
}

/// Characters a text field may not hold: each would end the line it is
/// written on (systemd) or the string it is passed as (Windows).
fn has_control(text: &str) -> bool {
    text.chars().any(char::is_control)
}

/// What is wrong with a spec before anything is written, in words.
pub fn check(spec: &ServiceSpec, creating: bool) -> Result<(), String> {
    if creating {
        imp::check_name(&spec.name)?;
    }
    let fields = [
        ("The name shown", &spec.display),
        ("The description", &spec.description),
        ("The program", &spec.program),
        ("The recovery program", &spec.recovery.program),
    ];
    for (what, text) in fields {
        if has_control(text) {
            return Err(format!(
                "{what} has a line break or a control character in it"
            ));
        }
    }
    if spec.program.trim().is_empty() {
        return Err("It needs a program to run".into());
    }
    if let Account::User(name) = &spec.account {
        if name.trim().is_empty() {
            return Err("Name the account it runs as".into());
        }
        if has_control(name) {
            return Err("The account name has a control character in it".into());
        }
    }
    for dependency in &spec.dependencies {
        if dependency.trim().is_empty() || has_control(dependency) {
            return Err(format!("\"{dependency}\" is not a service name"));
        }
    }
    if spec.recovery.actions.contains(&FailureAction::RunProgram)
        && spec.recovery.program.trim().is_empty()
    {
        return Err("Say which program to run when it fails".into());
    }
    Ok(())
}

/// Every service now, read on the calling thread.
pub fn query() -> ServiceList {
    imp::list(&mut HashMap::new())
}

/// One service's full settings, read on the calling thread.
pub fn detail(name: &str) -> Result<ServiceDetail, String> {
    imp::detail(name)
}

/// Carries out a change on the calling thread. On Linux a system service's
/// change asks for an administrator's password (pkexec), so this may wait
/// for the user.
pub fn change_now(change: &ServiceChange) -> ChangeOutcome {
    match change {
        ServiceChange::Create(spec) => match check(spec, true) {
            Ok(()) => imp::create(spec),
            Err(e) => ChangeOutcome::refused(e),
        },
        ServiceChange::Configure { before, after } => {
            if let Err(e) = check(after, false) {
                return ChangeOutcome::refused(e);
            }
            if before == after {
                return ChangeOutcome {
                    result: Ok(format!("Nothing changed for {}", before.display)),
                    written: false,
                };
            }
            imp::configure(before, after)
        }
        ServiceChange::Delete {
            name,
            display,
            stop,
        } => imp::delete(name, display, *stop),
    }
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

    /// Reads a service's full settings on a thread of its own; the answer
    /// comes as `ServiceEvent::Detail`.
    pub fn detail(&self, name: Arc<str>) {
        let (events, notify) = (self.events.clone(), self.notify.clone());
        let _ = thread::Builder::new()
            .name("anabolic-service-detail".into())
            .spawn(move || {
                let detail = imp::detail(&name).map(Box::new);
                let _ = events.send(ServiceEvent::Detail { name, detail });
                notify();
            });
    }

    /// Creates, changes or deletes a service on a thread of its own; the
    /// outcome comes as `ServiceEvent::Changed`.
    pub fn change(&self, change: ServiceChange) {
        let (tx, events, notify) = (self.tx.clone(), self.events.clone(), self.notify.clone());
        let _ = thread::Builder::new()
            .name("anabolic-service-change".into())
            .spawn(move || {
                let name = change.name();
                let outcome = change_now(&change);
                let _ = tx.send(Request::Forget(name.clone()));
                let _ = events.send(ServiceEvent::Changed { name, outcome });
                let _ = tx.send(Request::Refresh);
                notify();
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_what_would_break_a_line() {
        let good = ServiceSpec {
            name: if cfg!(windows) {
                "AnabolicSpec".into()
            } else {
                "anabolic-spec.service".into()
            },
            display: "Anabolic spec".into(),
            program: "/bin/true".into(),
            ..Default::default()
        };
        assert_eq!(check(&good, true), Ok(()));
        let mut bad = good.clone();
        bad.display = "two\nlines".into();
        assert!(check(&bad, true).is_err());
        let mut bad = good.clone();
        bad.program = "  ".into();
        assert!(check(&bad, true).is_err());
        let mut bad = good.clone();
        bad.account = Account::User(String::new());
        assert!(check(&bad, true).is_err());
        let mut bad = good.clone();
        bad.recovery.actions[1] = FailureAction::RunProgram;
        assert!(check(&bad, true).is_err());
        let mut bad = good;
        bad.name = "a/b".into();
        assert!(check(&bad, true).is_err());
        assert!(
            format!("{:?}", Password("secret".into()))
                .find("secret")
                .is_none()
        );
    }
}
