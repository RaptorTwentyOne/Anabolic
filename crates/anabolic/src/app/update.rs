// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Updates in the app (see anabolic_core::update): a check soon after the
//! start and then once a day, what it finds installed or announced as
//! Settings says, and the bar that tells of it.

use std::time::SystemTime;

use anabolic_core::update::{Event, Release, Updater};
use egui::{Align, Layout};

use super::*;
use crate::settings::Updates;

/// The first check waits for the start to settle.
const FIRST_CHECK: Duration = Duration::from_secs(10);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
/// After a failed check (offline, say).
const RETRY: Duration = Duration::from_secs(60 * 60);

/// When Install automatically tries again after its `failures`th failed
/// install: most failures pass (a download that broke off, a release just
/// out and not yet everywhere), so after a minute, five and thirty; then
/// the next day's check.
fn install_retry(failures: u32) -> Option<Duration> {
    match failures {
        1 => Some(Duration::from_secs(60)),
        2 => Some(Duration::from_secs(5 * 60)),
        3 => Some(Duration::from_secs(30 * 60)),
        _ => None,
    }
}

pub(super) struct UpdateState {
    /// None for a copy built from source: it is updated from its source.
    updater: Option<Updater>,
    events: Receiver<Event>,
    next_check: Instant,
    checking: bool,
    checked_at: Option<SystemTime>,
    check_error: Option<String>,
    /// A newer release, not installed yet.
    available: Option<Release>,
    /// Downloading it: bytes so far, of how many (0 before it starts).
    progress: Option<(u64, u64)>,
    install_error: Option<String>,
    /// Failed installs in a row, and when Install automatically tries
    /// again.
    failures: u32,
    retry_at: Option<Instant>,
    /// Installed: it runs from the next start.
    installed: Option<Release>,
    /// "Update and restart": restart once installed.
    restart_when_installed: bool,
    restarting: bool,
    /// The bar was closed; it comes back with the next news.
    dismissed: bool,
    /// Started with `--new` (a test instance): so is its successor.
    new_instance: bool,
    /// ANABOLIC_UPDATE_TEST=<version>: this copy takes itself for that
    /// version, and restarts as soon as a release is installed (tests of
    /// the whole round, against the real releases).
    test: bool,
}

impl UpdateState {
    pub(super) fn new(ctx: &egui::Context) -> Self {
        let test = std::env::var("ANABOLIC_UPDATE_TEST").ok();
        let current = test
            .clone()
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        let ctx = ctx.clone();
        let (updater, events) = match anabolic_core::update::this_exe()
            .filter(|exe| !anabolic_core::update::from_source(exe))
        {
            Some(exe) => {
                let (updater, events) =
                    Updater::start(current, exe, move || crate::stalls::wake(&ctx));
                (Some(updater), events)
            }
            None => (None, crossbeam_channel::never()),
        };
        Self {
            updater,
            events,
            next_check: Instant::now() + FIRST_CHECK,
            checking: false,
            checked_at: None,
            check_error: None,
            available: None,
            progress: None,
            install_error: None,
            failures: 0,
            retry_at: None,
            installed: None,
            restart_when_installed: false,
            restarting: false,
            dismissed: false,
            new_instance: std::env::args().any(|a| a == "--new"),
            test: test.is_some(),
        }
    }

    /// Something for the bar to say.
    pub(super) fn bar_wanted(&self) -> bool {
        !self.dismissed && (self.available.is_some() || self.installed.is_some() || self.restarting)
    }

    fn check(&mut self) {
        if let Some(updater) = &self.updater {
            self.checking = true;
            self.next_check = Instant::now() + DAY;
            updater.check();
        }
    }

    fn install(&mut self, release: Release) {
        if let Some(updater) = &self.updater {
            self.install_error = None;
            self.retry_at = None;
            self.progress = Some((0, 0));
            updater.install(release);
        }
    }

    /// "could not be installed: why", with when it is tried again first: the
    /// reason can be long (a path), and the bar cuts what does not fit.
    fn failed_text(&self, e: &str, auto: bool) -> String {
        match self.retry_at {
            Some(at) if auto => {
                let minutes = at
                    .saturating_duration_since(Instant::now())
                    .as_secs()
                    .div_ceil(60)
                    .max(1);
                format!("could not be installed (trying again in {minutes} min): {e}")
            }
            _ => format!("could not be installed: {e}"),
        }
    }

    fn restart(&mut self) {
        if let Some(updater) = &self.updater {
            self.restarting = true;
            updater.restart(if self.new_instance {
                vec!["--new".to_string()]
            } else {
                Vec::new()
            });
        }
    }

    /// Installs the release that is out, and restarts into it.
    fn update_and_restart(&mut self) {
        if let Some(release) = self.available.clone() {
            self.restart_when_installed = true;
            self.install(release);
        }
    }
}

impl App {
    /// Runs in `logic`: with the window closed too.
    pub(super) fn update_logic(&mut self, ctx: &egui::Context) {
        let events: Vec<Event> = self.update.events.try_iter().collect();
        for event in events {
            let u = &mut self.update;
            match event {
                Event::Checked(result) => {
                    u.checking = false;
                    u.checked_at = Some(SystemTime::now());
                    match result {
                        Ok(Some(release)) => {
                            u.check_error = None;
                            let known =
                                u.available.as_ref().map(|r| &r.version) == Some(&release.version);
                            if !known {
                                u.dismissed = false;
                            }
                            // A day later: try the install again.
                            u.install_error = None;
                            u.failures = 0;
                            u.retry_at = None;
                            u.available = Some(release);
                        }
                        Ok(None) => u.check_error = None,
                        Err(e) => {
                            u.check_error = Some(e);
                            u.next_check = Instant::now() + RETRY;
                        }
                    }
                }
                Event::Progress(done, total) => u.progress = Some((done, total)),
                Event::Installed(release) => {
                    u.progress = None;
                    u.failures = 0;
                    u.available = None;
                    u.dismissed = false;
                    if self.hidden
                        && let Some(tray) = &self.tray
                    {
                        tray.notify(
                            &format!("Anabolic {} is installed", release.version),
                            "It runs from the next start. Open Anabolic to restart into it now.",
                            false,
                        );
                    }
                    u.installed = Some(release);
                    if std::mem::take(&mut u.restart_when_installed) || u.test {
                        u.restart();
                    }
                }
                Event::InstallFailed(release, e) => {
                    u.progress = None;
                    u.restart_when_installed = false;
                    u.dismissed = false;
                    u.install_error = Some(e);
                    u.failures += 1;
                    u.retry_at = install_retry(u.failures).map(|d| Instant::now() + d);
                    u.available = Some(release);
                }
                Event::Restarting => {
                    // A real quit, not a close into the notification area:
                    // the new copy waits for this one to end.
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    ctx.request_repaint();
                }
                Event::RestartFailed(e) => {
                    u.restarting = false;
                    self.toasts.error(e);
                }
            }
        }
        let u = &mut self.update;
        let auto = self.settings.updates == Updates::Install;
        let retry = u.retry_at.is_some_and(|at| Instant::now() >= at);
        if auto
            && u.progress.is_none()
            && (u.install_error.is_none() || retry)
            && let Some(release) = u.available.clone()
        {
            u.install(release);
        }
        if self.settings.updates != Updates::Off
            && !u.checking
            && u.progress.is_none()
            && Instant::now() >= u.next_check
        {
            u.check();
        }
    }

    /// The bar under the top bar: a release that is out, on its way, or
    /// installed.
    pub(super) fn update_bar(&mut self, ui: &mut egui::Ui) {
        let mut open_page = None;
        let mut dismiss = false;
        let auto = self.settings.updates == Updates::Install;
        let u = &mut self.update;
        let release = u.installed.as_ref().or(u.available.as_ref()).cloned();
        let Some(release) = release else { return };
        let mut act = None;
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .small_button(widgets::icon(theme::icon::CLOSE, 10.0))
                .on_hover_text("Hide until there is news")
                .clicked()
            {
                dismiss = true;
            }
            if ui.small_button("What's new").clicked() {
                open_page = Some(release.page.clone());
            }
            // None while it downloads: the text says how far.
            if u.progress.is_none() {
                let action = if u.installed.is_some() {
                    "Restart now"
                } else {
                    "Update and restart"
                };
                if ui
                    .add_enabled(!u.restarting, egui::Button::new(action).small())
                    .clicked()
                {
                    act = Some(u.installed.is_some());
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let (color, text) = if u.restarting {
                    (
                        theme::TEXT,
                        format!("Restarting into Anabolic {}…", release.version),
                    )
                } else if let Some(r) = &u.installed {
                    (
                        theme::GOOD,
                        format!(
                            "Anabolic {} is installed: it runs from the next start.",
                            r.version
                        ),
                    )
                } else if let Some((done, total)) = u.progress {
                    let pct = (done * 100).checked_div(total).unwrap_or(0);
                    (
                        theme::TEXT,
                        format!("Downloading Anabolic {}…  {pct} %", release.version),
                    )
                } else if let Some(e) = &u.install_error {
                    (
                        theme::ELEVATED_TEXT,
                        format!("Anabolic {} {}", release.version, u.failed_text(e, auto)),
                    )
                } else {
                    (theme::TEXT, format!("Anabolic {} is out.", release.version))
                };
                ui.label(widgets::icon(theme::icon::UPDATE, 13.0).color(color));
                ui.add(egui::Label::new(RichText::new(text).color(theme::TEXT)).truncate());
            });
        });
        match act {
            Some(true) => u.restart(),
            Some(false) => u.update_and_restart(),
            None => {}
        }
        if dismiss {
            u.dismissed = true;
        }
        if let Some(url) = open_page {
            self.actions.run(Action::OpenUrl(url));
        }
    }

    /// The Settings page's section.
    pub(super) fn updates_section(&mut self, ui: &mut egui::Ui) {
        super::settings_page::heading(ui, "Updates");
        if self.update.updater.is_none() {
            super::settings_page::row(
                ui,
                "Built from source",
                "This copy was built with cargo: it updates with its source (git pull, then \
                 cargo build), not from the releases.",
                |_| {},
            );
            return;
        }
        super::settings_page::row(
            ui,
            "New releases",
            "Anabolic asks GitHub whether a newer release is out: soon after it starts, then \
             once a day. The question says nothing about this PC. A release is checked against \
             the checksum GitHub lists for it before it replaces this copy, and runs from the \
             next start.",
            |ui| {
                egui::ComboBox::from_id_salt("settings-updates")
                    .selected_text(self.settings.updates.label())
                    .width(180.0)
                    .show_ui(ui, |ui| {
                        for choice in Updates::ALL {
                            ui.selectable_value(&mut self.settings.updates, choice, choice.label());
                        }
                    });
            },
        );
        let auto = self.settings.updates == Updates::Install;
        let u = &mut self.update;
        let (state, about) = if u.restarting {
            ("Restarting…".to_string(), String::new())
        } else if let Some(r) = &u.installed {
            (
                format!("Anabolic {} is installed", r.version),
                "It runs from the next start.".to_string(),
            )
        } else if let (Some(r), Some((done, total))) = (&u.available, u.progress) {
            let pct = (done * 100).checked_div(total).unwrap_or(0);
            (
                format!("Downloading Anabolic {}: {pct} %", r.version),
                String::new(),
            )
        } else if let Some(r) = &u.available {
            (
                format!("Anabolic {} is out", r.version),
                u.install_error
                    .as_ref()
                    .map_or_else(String::new, |e| format!("It {}", u.failed_text(e, auto))),
            )
        } else if u.checking {
            ("Checking…".to_string(), String::new())
        } else {
            let when = u
                .checked_at
                .map(|t| format!("Last checked at {}.", fmt::clock(t, false)));
            match (&u.check_error, when) {
                (Some(e), _) => ("The last check failed".to_string(), format!("{e}.")),
                (None, Some(when)) => ("This is the latest release".to_string(), when),
                (None, None) => ("Not checked yet".to_string(), String::new()),
            }
        };
        let mut act = None;
        super::settings_page::row(ui, &state, &about, |ui| {
            let busy = u.checking || u.progress.is_some() || u.restarting;
            let action = if u.installed.is_some() {
                Some((true, "Restart now"))
            } else if u.available.is_some() {
                Some((false, "Update and restart"))
            } else {
                None
            };
            match action {
                Some((installed, label)) => {
                    if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                        act = Some(installed);
                    }
                }
                None => {
                    if ui
                        .add_enabled(!busy, egui::Button::new("Check now"))
                        .clicked()
                    {
                        u.check();
                    }
                }
            }
        });
        match act {
            Some(true) => u.restart(),
            Some(false) => u.update_and_restart(),
            None => {}
        }
    }
}
