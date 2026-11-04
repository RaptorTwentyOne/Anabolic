// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Services page's dialog: a new service, a service's properties, and
//! deleting one. What it shows comes from the service manager on a thread
//! of the services' own (`ServiceWatch::detail`); what it asks for is
//! carried out there too (`ServiceWatch::change`).

use std::sync::Arc;
use std::time::Duration;

use anabolic_core::services::{
    Account, ErrorControl, FailureAction, Password, Recovery, ServiceChange, ServiceDetail,
    ServiceSpec, StartType,
};
use eframe::egui::{self, Id, Modal, RichText};

use crate::{theme, widgets};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Purpose {
    New,
    Edit,
    Delete,
}

/// The text field the Open dialog fills.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Program,
    RecoveryProgram,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Who {
    System,
    LocalService,
    NetworkService,
    User,
}

/// A failure's delay: in minutes on Windows, as the Services console has
/// it; in seconds on Linux, as systemd has it.
const DELAY_UNIT: f64 = if cfg!(windows) { 60.0 } else { 1.0 };

/// What the dialog's fields hold.
struct Form {
    name: String,
    display: String,
    description: String,
    program: String,
    start: StartType,
    who: Who,
    user: String,
    password: String,
    confirm: String,
    /// A user account that stays: give it a new password (else it keeps
    /// the one it has).
    new_password: bool,
    dependencies: String,
    error_control: ErrorControl,
    actions: [FailureAction; 3],
    delay: f64,
    reset_days: u32,
    reset_never: bool,
    recovery_program: String,
    also: bool,
    /// Linux: one of the user's own units.
    own: bool,
}

impl Form {
    fn from_spec(spec: &ServiceSpec) -> Self {
        let (who, user) = match &spec.account {
            Account::System => (Who::System, String::new()),
            Account::LocalService => (Who::LocalService, String::new()),
            Account::NetworkService => (Who::NetworkService, String::new()),
            Account::User(name) => (Who::User, name.clone()),
        };
        Self {
            name: spec.name.clone(),
            display: spec.display.clone(),
            description: spec.description.clone(),
            program: spec.program.clone(),
            start: spec.start,
            who,
            user,
            password: String::new(),
            confirm: String::new(),
            new_password: false,
            dependencies: spec.dependencies.join(", "),
            error_control: spec.error_control,
            actions: spec.recovery.actions,
            delay: spec.recovery.delay.as_secs_f64() / DELAY_UNIT,
            reset_days: spec.recovery.reset_days.unwrap_or(0),
            reset_never: spec.recovery.reset_days.is_none(),
            recovery_program: spec.recovery.program.clone(),
            also: spec.recovery.also,
            own: spec.own,
        }
    }

    fn account(&self) -> Account {
        match self.who {
            Who::System => Account::System,
            Who::LocalService => Account::LocalService,
            Who::NetworkService => Account::NetworkService,
            Who::User => Account::User(self.user.trim().to_string()),
        }
    }

    /// The spec the fields say, over `base` (what was read, or a new
    /// service's defaults): what the dialog does not show stays as it was.
    fn spec(&self, base: &ServiceSpec, creating: bool) -> Result<ServiceSpec, String> {
        let account = self.account();
        let password = if cfg!(windows)
            && self.who == Who::User
            && (account != base.account || creating || self.new_password)
        {
            if self.password != self.confirm {
                return Err("The two passwords are not the same".into());
            }
            Some(Password(self.password.clone()))
        } else {
            None
        };
        let separators: &[char] = if cfg!(windows) {
            &[',', ';']
        } else {
            &[',', ';', ' ', '\t']
        };
        let dependencies = self
            .dependencies
            .split(separators)
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(String::from)
            .collect();
        let actions = if cfg!(windows) {
            self.actions
        } else {
            [self.actions[0]; 3]
        };
        let recovery = Recovery {
            actions,
            delay: Duration::from_secs_f64((self.delay * DELAY_UNIT).max(0.0)),
            reset_days: if cfg!(windows) {
                (!self.reset_never).then_some(self.reset_days)
            } else {
                base.recovery.reset_days
            },
            program: if cfg!(windows) {
                self.recovery_program.trim().to_string()
            } else {
                base.recovery.program.clone()
            },
            also: self.also,
        };
        // Unchanged, a delay that is not used stays as it was read (it may
        // be finer than the field shows).
        let recovery = if recovery.actions == base.recovery.actions
            && (recovery.delay.as_secs_f64() - base.recovery.delay.as_secs_f64()).abs() < 0.001
        {
            Recovery {
                delay: base.recovery.delay,
                ..recovery
            }
        } else {
            recovery
        };
        Ok(ServiceSpec {
            name: if creating {
                self.name.trim().to_string()
            } else {
                base.name.clone()
            },
            display: self.display.trim().to_string(),
            description: if cfg!(windows) {
                self.description.trim().to_string()
            } else {
                base.description.clone()
            },
            program: self.program.trim().to_string(),
            start: self.start,
            account,
            password,
            dependencies,
            error_control: if cfg!(windows) {
                self.error_control
            } else {
                base.error_control
            },
            recovery,
            own: if creating { self.own } else { base.own },
        })
    }
}

pub struct ServiceDialog {
    pub purpose: Purpose,
    /// The service it is about; None for a new one.
    pub name: Option<Arc<str>>,
    pub display: Arc<str>,
    /// Its settings as read; None while they are read.
    pub detail: Option<Result<ServiceDetail, String>>,
    form: Form,
    /// A change is on its way.
    pub busy: bool,
    /// What went wrong, and what to do about it.
    pub error: Option<(String, Option<String>)>,
    /// The Open dialog is up for this field.
    pub browsing: Option<Field>,
    elevated: bool,
}

pub enum ServiceChoice {
    Pending,
    Apply(ServiceChange),
    /// Set it to Disabled (Masked) instead: it cannot be deleted.
    Disable,
    Browse(Field),
    Cancel,
}

/// The settings a new service starts from.
fn new_spec() -> ServiceSpec {
    ServiceSpec {
        start: StartType::Manual,
        recovery: Recovery {
            delay: Duration::from_secs(if cfg!(windows) { 60 } else { 5 }),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn note(ui: &mut egui::Ui, text: impl Into<String>, color: egui::Color32) {
    ui.label(RichText::new(text.into()).size(theme::META).color(color));
}

fn label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(theme::TEXT_WEAK));
}

/// An empty field's hint: dim and slanted, never taken for a value.
fn hint(text: &str) -> RichText {
    RichText::new(text).color(theme::TEXT_DIM).italics()
}

impl ServiceDialog {
    pub fn create(elevated: bool) -> Self {
        Self {
            purpose: Purpose::New,
            name: None,
            display: "".into(),
            detail: None,
            form: Form::from_spec(&new_spec()),
            busy: false,
            error: None,
            browsing: None,
            elevated,
        }
    }

    /// Opens on a service whose settings are being read.
    pub fn open(purpose: Purpose, name: Arc<str>, display: Arc<str>, elevated: bool) -> Self {
        Self {
            purpose,
            name: Some(name),
            display,
            detail: None,
            form: Form::from_spec(&new_spec()),
            busy: false,
            error: None,
            browsing: None,
            elevated,
        }
    }

    /// Its settings arrived.
    pub fn loaded(&mut self, detail: Result<ServiceDetail, String>) {
        if let Ok(found) = &detail {
            self.form = Form::from_spec(&found.spec);
            self.display = found.spec.display.as_str().into();
        }
        self.detail = Some(detail);
    }

    /// A file chosen in the Open dialog.
    pub fn fill(&mut self, field: Field, path: String) {
        let path = if path.contains(' ') {
            format!("\"{path}\"")
        } else {
            path
        };
        match field {
            Field::Program => self.form.program = path,
            Field::RecoveryProgram => self.form.recovery_program = path,
        }
    }

    /// Whether this service may be written to from here: Windows needs an
    /// administrator; on Linux pkexec asks for one when it has to.
    fn can_write(&self) -> bool {
        !cfg!(windows) || self.elevated
    }

    /// `running`: the service runs now, as the list last said.
    pub fn show(&mut self, ctx: &egui::Context, running: bool) -> ServiceChoice {
        let mut choice = ServiceChoice::Pending;
        let response = Modal::new(Id::new("service-dialog")).show(ctx, |ui| {
            ui.set_width(580.0);
            let title = match self.purpose {
                Purpose::New => "New service".to_string(),
                Purpose::Edit => format!("Properties of {}", self.display),
                Purpose::Delete => format!("Delete {}?", self.display),
            };
            ui.label(widgets::strong(title, theme::SUBTITLE));
            // The properties name it in their first row.
            if let Some(name) = &self.name
                && **name != *self.display
                && self.purpose == Purpose::Delete
            {
                ui.label(RichText::new(name.to_string()).color(theme::TEXT_WEAK));
            }
            ui.add_space(8.0);
            let detail = self.detail.clone();
            match (self.purpose, detail) {
                (Purpose::New, _) => self.form(ui, None, &mut choice),
                (_, None) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Reading its settings…").color(theme::TEXT_WEAK));
                    });
                    ui.add_space(10.0);
                    if ui.button("Cancel").clicked() {
                        choice = ServiceChoice::Cancel;
                    }
                }
                (_, Some(Err(e))) => {
                    ui.label(
                        RichText::new(format!("Its settings could not be read: {e}"))
                            .color(theme::BAD),
                    );
                    ui.add_space(10.0);
                    if ui.button("Close").clicked() {
                        choice = ServiceChoice::Cancel;
                    }
                }
                (Purpose::Edit, Some(Ok(found))) => self.form(ui, Some(&found), &mut choice),
                (Purpose::Delete, Some(Ok(found))) => self.delete(ui, &found, running, &mut choice),
            }
        });
        if matches!(choice, ServiceChoice::Pending)
            && response.should_close()
            && !self.busy
            && self.browsing.is_none()
        {
            choice = ServiceChoice::Cancel;
        }
        choice
    }

    /// The error of the last try, and the rights it takes.
    fn footer_notes(&self, ui: &mut egui::Ui, system_unit: bool) {
        if cfg!(windows) && !self.elevated {
            note(
                ui,
                "Changing services takes administrator rights: Settings has Restart as admin.",
                theme::ELEVATED_TEXT,
            );
        } else if !cfg!(windows) && system_unit {
            note(
                ui,
                "systemd's own services are written by root: pkexec asks for an \
                 administrator's password.",
                theme::TEXT_DIM,
            );
        }
        if let Some((error, next)) = &self.error {
            ui.add_space(4.0);
            ui.label(RichText::new(error).color(theme::BAD));
            if let Some(next) = next {
                note(ui, next.clone(), theme::TEXT_WEAK);
            }
        }
    }

    fn form(
        &mut self,
        ui: &mut egui::Ui,
        found: Option<&ServiceDetail>,
        choice: &mut ServiceChoice,
    ) {
        let creating = found.is_none();
        let fixed = found.and_then(|d| d.fixed.clone());
        let own = if creating {
            self.form.own
        } else {
            found.is_some_and(|d| d.spec.own)
        };
        let browsing = self.browsing.is_some();
        let max_height = (ui.ctx().content_rect().height() - 220.0).max(240.0);
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let f = &mut self.form;
                egui::Grid::new("service-general")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .min_col_width(120.0)
                    .show(ui, |ui| {
                        label(ui, "Name");
                        if creating {
                            ui.add(
                                egui::TextEdit::singleline(&mut f.name)
                                    .hint_text(hint(if cfg!(windows) {
                                        "a name without spaces is easiest to type"
                                    } else {
                                        ".service is added when it is not there"
                                    }))
                                    .desired_width(380.0),
                            );
                        } else {
                            ui.label(RichText::new(&f.name).color(theme::TEXT_WEAK));
                        }
                        ui.end_row();
                        label(
                            ui,
                            if cfg!(windows) {
                                "Display name"
                            } else {
                                "Description"
                            },
                        );
                        ui.add(egui::TextEdit::singleline(&mut f.display).desired_width(380.0));
                        ui.end_row();
                        if cfg!(windows) {
                            label(ui, "Description");
                            ui.add(
                                egui::TextEdit::multiline(&mut f.description)
                                    .desired_rows(2)
                                    .desired_width(380.0),
                            );
                            ui.end_row();
                        }
                        label(ui, "Program");
                        ui.horizontal(|ui| {
                            ui.add_enabled(
                                fixed.is_none(),
                                egui::TextEdit::singleline(&mut f.program)
                                    .hint_text(hint(if cfg!(windows) {
                                        r#"for example "C:\Program Files\App\svc.exe" --run"#
                                    } else {
                                        "for example /usr/local/bin/svc --run"
                                    }))
                                    .desired_width(if cfg!(windows) { 300.0 } else { 380.0 }),
                            );
                            if cfg!(windows)
                                && ui
                                    .add_enabled(
                                        fixed.is_none() && !browsing,
                                        egui::Button::new("Browse\u{2026}"),
                                    )
                                    .clicked()
                            {
                                *choice = ServiceChoice::Browse(Field::Program);
                            }
                        });
                        ui.end_row();
                        label(ui, "Startup");
                        if matches!(f.start, StartType::Boot | StartType::System) {
                            ui.label(f.start.label()).on_hover_text(
                                "It starts when another unit needs it; there is nothing to switch.",
                            );
                        } else {
                            let choices: &[StartType] = if creating && !cfg!(windows) {
                                &[StartType::Automatic, StartType::Manual]
                            } else {
                                &StartType::CHOICES
                            };
                            egui::ComboBox::from_id_salt("service-start")
                                .selected_text(f.start.label())
                                .show_ui(ui, |ui| {
                                    for &start in choices {
                                        ui.selectable_value(&mut f.start, start, start.label());
                                    }
                                });
                        }
                        ui.end_row();
                        if creating && !cfg!(windows) {
                            ui.label("");
                            ui.checkbox(&mut f.own, "Only for me").on_hover_text(
                                "Your own systemd runs it as you, from when you sign in, and \
                                     no password is asked for. Without this, the system's runs it \
                                     from boot.",
                            );
                            ui.end_row();
                        }
                    });

                ui.add_space(10.0);
                widgets::section(
                    ui,
                    if cfg!(windows) {
                        "Log on as"
                    } else {
                        "Runs as"
                    },
                );
                if !cfg!(windows) && own {
                    note(ui, "You: your own services run as you.", theme::TEXT_WEAK);
                } else {
                    ui.add_enabled_ui(fixed.is_none(), |ui| {
                        if cfg!(windows) {
                            ui.radio_value(
                                &mut f.who,
                                Who::System,
                                "Local System: every right on this computer",
                            );
                            ui.radio_value(
                                &mut f.who,
                                Who::LocalService,
                                "Local Service: few rights, anonymous on the network",
                            );
                            ui.radio_value(
                                &mut f.who,
                                Who::NetworkService,
                                "Network Service: few rights, the computer's own name on the \
                                 network",
                            );
                        } else {
                            ui.radio_value(&mut f.who, Who::System, "root");
                        }
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut f.who, Who::User, "This account");
                            ui.add_enabled(
                                f.who == Who::User,
                                egui::TextEdit::singleline(&mut f.user)
                                    .hint_text(hint(if cfg!(windows) {
                                        r".\name or DOMAIN\name"
                                    } else {
                                        "a user name"
                                    }))
                                    .desired_width(260.0),
                            );
                        });
                        if cfg!(windows) && f.who == Who::User {
                            let same = found.is_some_and(|d| {
                                d.spec.account == Account::User(f.user.trim().to_string())
                            });
                            if same {
                                ui.checkbox(&mut f.new_password, "Give it a new password");
                            }
                            if !same || f.new_password {
                                egui::Grid::new("service-password")
                                    .num_columns(2)
                                    .spacing([12.0, 6.0])
                                    .min_col_width(120.0)
                                    .show(ui, |ui| {
                                        label(ui, "Password");
                                        ui.add(
                                            egui::TextEdit::singleline(&mut f.password)
                                                .password(true)
                                                .desired_width(260.0),
                                        );
                                        ui.end_row();
                                        label(ui, "Once more");
                                        ui.add(
                                            egui::TextEdit::singleline(&mut f.confirm)
                                                .password(true)
                                                .desired_width(260.0),
                                        );
                                        ui.end_row();
                                    });
                                note(
                                    ui,
                                    "The account is given the right to log on as a service, as \
                                     the Services console does.",
                                    theme::TEXT_DIM,
                                );
                            }
                        }
                    });
                }

                ui.add_space(10.0);
                widgets::section(ui, "Needs");
                note(
                    ui,
                    if cfg!(windows) {
                        "Services that must run before it, by name, separated by commas; \
                         +Name for a group."
                    } else {
                        "Units that must run with it, separated by commas or spaces; it starts \
                         after them. A bare name is a service's."
                    },
                    theme::TEXT_DIM,
                );
                ui.add(
                    egui::TextEdit::singleline(&mut f.dependencies)
                        .hint_text(hint("none"))
                        .desired_width(f32::INFINITY),
                );

                ui.add_space(10.0);
                widgets::section(ui, "If it fails");
                if cfg!(windows) {
                    egui::Grid::new("service-recovery")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .min_col_width(120.0)
                        .show(ui, |ui| {
                            for (i, when) in ["First failure", "Second failure", "Later failures"]
                                .iter()
                                .enumerate()
                            {
                                label(ui, when);
                                if f.actions[i] == FailureAction::Reboot {
                                    ui.label(FailureAction::Reboot.label()).on_hover_text(
                                        "Anabolic does not set this; it stays as it is.",
                                    );
                                } else {
                                    egui::ComboBox::from_id_salt(("service-failure", i))
                                        .selected_text(f.actions[i].label())
                                        .show_ui(ui, |ui| {
                                            for action in [
                                                FailureAction::Nothing,
                                                FailureAction::Restart,
                                                FailureAction::RunProgram,
                                            ] {
                                                ui.selectable_value(
                                                    &mut f.actions[i],
                                                    action,
                                                    action.label(),
                                                );
                                            }
                                        });
                                }
                                ui.end_row();
                            }
                            let acts = f.actions.iter().any(|a| *a != FailureAction::Nothing);
                            label(ui, "After");
                            ui.add_enabled(
                                acts,
                                egui::DragValue::new(&mut f.delay)
                                    .range(0.0..=1440.0)
                                    .speed(0.25)
                                    .max_decimals(2)
                                    .suffix(" minutes"),
                            );
                            ui.end_row();
                            label(ui, "Count again after");
                            ui.horizontal(|ui| {
                                ui.add_enabled(
                                    acts && !f.reset_never,
                                    egui::DragValue::new(&mut f.reset_days)
                                        .range(0..=9999)
                                        .suffix(" days"),
                                );
                                ui.add_enabled(
                                    acts,
                                    egui::Checkbox::new(&mut f.reset_never, "never"),
                                );
                            });
                            ui.end_row();
                            let runs = f.actions.contains(&FailureAction::RunProgram);
                            label(ui, "Program to run");
                            ui.horizontal(|ui| {
                                ui.add_enabled(
                                    runs,
                                    egui::TextEdit::singleline(&mut f.recovery_program)
                                        .desired_width(300.0),
                                );
                                if ui
                                    .add_enabled(
                                        runs && !browsing,
                                        egui::Button::new("Browse\u{2026}"),
                                    )
                                    .clicked()
                                {
                                    *choice = ServiceChoice::Browse(Field::RecoveryProgram);
                                }
                            });
                            ui.end_row();
                        });
                    ui.checkbox(
                        &mut f.also,
                        "Also when it stops with an error of its own, not only when its process \
                         ends unexpectedly",
                    );
                    ui.add_space(6.0);
                    egui::Grid::new("service-error-control")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .min_col_width(120.0)
                        .show(ui, |ui| {
                            label(ui, "At boot, a failure");
                            let mut choices = ErrorControl::CHOICES.to_vec();
                            if f.error_control == ErrorControl::Critical {
                                choices.push(ErrorControl::Critical);
                            }
                            egui::ComboBox::from_id_salt("service-error")
                                .selected_text(f.error_control.label())
                                .show_ui(ui, |ui| {
                                    for control in choices {
                                        ui.selectable_value(
                                            &mut f.error_control,
                                            control,
                                            control.label(),
                                        )
                                        .on_hover_text(
                                            match control {
                                                ErrorControl::Ignore => "is ignored",
                                                ErrorControl::Normal => "is logged",
                                                ErrorControl::Severe => {
                                                    "is logged, and Windows starts again with the \
                                                 last settings that worked"
                                                }
                                                ErrorControl::Critical => {
                                                    "is logged; Windows starts again with the last \
                                                 settings that worked, and stops if they fail too"
                                                }
                                            },
                                        );
                                    }
                                });
                            ui.end_row();
                        });
                } else {
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("service-restart")
                            .selected_text(match f.actions[0] {
                                FailureAction::Restart => "Restart it",
                                _ => "Do nothing",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut f.actions[0],
                                    FailureAction::Nothing,
                                    "Do nothing",
                                );
                                ui.selectable_value(
                                    &mut f.actions[0],
                                    FailureAction::Restart,
                                    "Restart it",
                                );
                            });
                        let restarts = f.actions[0] == FailureAction::Restart;
                        ui.label("after");
                        ui.add_enabled(
                            restarts,
                            egui::DragValue::new(&mut f.delay)
                                .range(0.0..=86400.0)
                                .speed(0.5)
                                .max_decimals(1)
                                .suffix(" s"),
                        );
                    });
                    ui.add_enabled(
                        f.actions[0] == FailureAction::Restart,
                        egui::Checkbox::new(
                            &mut f.also,
                            "Also when it ends without an error: keep it running",
                        ),
                    );
                }
            });

        ui.add_space(8.0);
        if let Some(why) = &fixed {
            note(ui, why.clone(), theme::ELEVATED_TEXT);
        }
        if let Some(found) = found
            && found.drop_in
            && !cfg!(windows)
        {
            note(
                ui,
                format!(
                    "Its unit file came with the system ({}); changes go into a drop-in beside \
                     it, as systemctl edit does.",
                    found.source
                ),
                theme::TEXT_DIM,
            );
        }
        self.footer_notes(ui, !own);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let label = if creating { "Create" } else { "Save" };
            if ui
                .add_enabled(
                    self.can_write() && !self.busy && !browsing,
                    egui::Button::new(widgets::strong(label, theme::BODY)),
                )
                .clicked()
            {
                let base = found.map_or_else(new_spec, |d| d.spec.clone());
                let made = self.form.spec(&base, creating).and_then(|spec| {
                    anabolic_core::services::check(&spec, creating).map(|()| spec)
                });
                match made {
                    Ok(spec) => {
                        self.error = None;
                        *choice = ServiceChoice::Apply(if creating {
                            ServiceChange::Create(Box::new(spec))
                        } else {
                            ServiceChange::Configure {
                                before: Box::new(base),
                                after: Box::new(spec),
                            }
                        });
                    }
                    Err(e) => self.error = Some((e, None)),
                }
            }
            if ui
                .add_enabled(!self.busy, egui::Button::new("Cancel"))
                .clicked()
            {
                *choice = ServiceChoice::Cancel;
            }
            if self.busy {
                ui.spinner();
            }
        });
    }

    fn delete(
        &mut self,
        ui: &mut egui::Ui,
        found: &ServiceDetail,
        running: bool,
        choice: &mut ServiceChoice,
    ) {
        let program = found.spec.program.trim();
        if let Some(keep) = &found.keep {
            ui.label(RichText::new(keep).color(theme::TEXT_WEAK));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if found.spec.start != StartType::Disabled
                    && ui
                        .add_enabled(
                            self.can_write(),
                            egui::Button::new(format!("Set it to {}", StartType::Disabled.label())),
                        )
                        .clicked()
                {
                    *choice = ServiceChoice::Disable;
                }
                if ui.button("Cancel").clicked() {
                    *choice = ServiceChoice::Cancel;
                }
            });
            return;
        }
        ui.label(if cfg!(windows) {
            format!(
                "Windows forgets the service and its settings. Its program stays where it is: \
                 {program}"
            )
        } else {
            format!(
                "Its unit file goes, with its drop-ins ({}), and systemd forgets it. Its program \
                 stays where it is: {program}",
                found.source
            )
        });
        if running {
            ui.add_space(4.0);
            ui.label(
                RichText::new("It is running: it is stopped first.").color(theme::ELEVATED_TEXT),
            );
        }
        if cfg!(windows) {
            note(
                ui,
                "If something holds it open (the Services console, for one), Windows removes it \
                 once that lets go.",
                theme::TEXT_DIM,
            );
        }
        ui.add_space(4.0);
        self.footer_notes(ui, !found.spec.own);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let delete =
                egui::Button::new(widgets::strong("Delete", theme::BODY).color(theme::TEXT))
                    .fill(theme::BAD_STRONG);
            if ui
                .add_enabled(self.can_write() && !self.busy, delete)
                .clicked()
                && let Some(name) = &self.name
            {
                self.error = None;
                *choice = ServiceChoice::Apply(ServiceChange::Delete {
                    name: name.clone(),
                    display: self.display.clone(),
                    stop: running,
                });
            }
            if ui
                .add_enabled(!self.busy, egui::Button::new("Cancel"))
                .clicked()
            {
                *choice = ServiceChoice::Cancel;
            }
            if self.busy {
                ui.spinner();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read() -> ServiceSpec {
        ServiceSpec {
            name: "Svc".into(),
            display: "Service".into(),
            description: "What it does".into(),
            program: "C:\\svc.exe".into(),
            account: Account::User(r".\worker".into()),
            dependencies: vec!["RpcSs".into(), "+Group".into()],
            recovery: Recovery {
                // systemd answers every failure alike.
                actions: if cfg!(windows) {
                    [
                        FailureAction::Restart,
                        FailureAction::Reboot,
                        FailureAction::Nothing,
                    ]
                } else {
                    [FailureAction::Restart; 3]
                },
                delay: Duration::from_millis(1234),
                reset_days: None,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn an_untouched_form_changes_nothing() {
        let before = read();
        let form = Form::from_spec(&before);
        let after = form.spec(&before, false).unwrap();
        assert_eq!(after, before);
    }

    #[test]
    fn a_new_account_asks_for_matching_passwords() {
        let before = read();
        let mut form = Form::from_spec(&before);
        form.who = Who::User;
        form.user = r".\other".into();
        form.password = "a".into();
        form.confirm = "b".into();
        let mismatch = form.spec(&before, false);
        if cfg!(windows) {
            assert!(mismatch.is_err());
            form.confirm = "a".into();
            let after = form.spec(&before, false).unwrap();
            assert_eq!(after.password, Some(Password("a".into())));
        } else {
            assert_eq!(mismatch.unwrap().password, None);
        }
        form.dependencies = "  ".into();
        assert!(form.spec(&before, false).unwrap().dependencies.is_empty());
    }
}
