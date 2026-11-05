// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The Startup page's boots: each start in its steps (the firmware, the
//! loader, the system to the sign-in screen, the sign-in to the desktop),
//! how the session before it ended and who asked, what failed as it
//! started, and how the session it began went. Read once from the system's
//! logs, in the background, and again on request.

use std::sync::Arc;
use std::time::Duration;

use anabolic_core::boot::{Boot, Boots, Ending, Shutdown};
use anabolic_core::sysinfo::{local_time, now_filetime};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Align, Align2, Color32, Label, Layout, Rect, RichText, Sense, Ui, vec2};

use crate::table::{Columns, Spec};
use crate::{panes, theme, widgets};

/// Boots read from the logs, and the reading under way.
#[derive(Default)]
pub struct BootsView {
    read: Option<Result<Arc<Boots>, String>>,
    reading: Option<Receiver<Result<Boots, String>>>,
    /// The boot shown in the pane, by when it began.
    selected: Option<i64>,
}

impl BootsView {
    fn start(&mut self, ctx: &egui::Context) {
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("boots".into())
            .spawn(move || {
                let _ = tx.send(anabolic_core::boot::read());
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.reading = Some(rx),
            Err(e) => self.read = Some(Err(format!("could not start reading: {e}"))),
        }
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.reading
            && let Ok(result) = rx.try_recv()
        {
            self.read = Some(result.map(Arc::new));
            self.reading = None;
        }
    }
}

/// The steps of a start, each with its colour.
const FIRMWARE: Color32 = theme::KERNEL;
const LOADER: Color32 = Color32::from_rgb(0x5E, 0x6B, 0x86);
const SYSTEM: Color32 = theme::CPU;
const WAIT: Color32 = Color32::from_rgb(0x3A, 0x40, 0x4C);
const DESKTOP: Color32 = theme::MEMORY;

/// The system's part, by what it ends at.
const SYSTEM_PART: &str = if cfg!(windows) {
    "Windows to the sign-in screen"
} else {
    "To the desktop"
};

/// Above this the chart stops a bar.
const CHART_CAP_MS: u64 = 120_000;

/// "16.2 s", "1 min 12 s", "1 h 9 min", "3 d 4 h".
fn secs(ms: u64) -> String {
    if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else if ms < 3_600_000 {
        format!("{} min {} s", ms / 60_000, ms / 1000 % 60)
    } else if ms < 2 * 86_400_000 {
        crate::fmt::span(Duration::from_millis(ms))
    } else {
        format!("{} d {} h", ms / 86_400_000, ms / 3_600_000 % 24)
    }
}

fn when(at: i64) -> String {
    local_time(at)
        .map(|t| t.chars().take(16).collect())
        .unwrap_or_default()
}

/// The same to the second.
fn when_exactly(at: i64) -> String {
    local_time(at)
        .map(|t| t.chars().take(19).collect())
        .unwrap_or_default()
}

/// The system's own part: loader and start to the sign-in screen, or to
/// the sign-in when the screen's moment is not known.
fn system_ms(b: &Boot) -> Option<u64> {
    let start = b.to_screen_ms.or(b.ready_ms)?;
    Some(b.loader_ms.unwrap_or(0) + start)
}

/// The whole start without anyone's wait: firmware, the system, and the
/// sign-in to the desktop.
fn together(b: &Boot) -> Option<u64> {
    b.to_screen_ms?;
    Some(b.firmware_ms? + system_ms(b)? + b.to_desktop_ms.unwrap_or(0))
}

/// Who asked for a shutdown, in words.
fn who(e: &Ending) -> Option<String> {
    let by = e.by.as_deref()?;
    let you = e
        .user
        .as_deref()
        .is_some_and(|u| !u.starts_with("NT AUTHORITY"));
    Some(match by.to_ascii_lowercase().as_str() {
        "trustedinstaller.exe" | "mousocoreworker.exe" | "usoclient.exe" | "wuauclt.exe" => {
            "Windows Update".into()
        }
        "msiexec.exe" => "Windows Installer".into(),
        "startmenuexperiencehost.exe" | "explorer.exe" | "shellexperiencehost.exe" => {
            "you, from the Start menu".into()
        }
        "winlogon.exe" if you => "you".into(),
        "winlogon.exe" => "Windows".into(),
        "shutdown.exe" => "the shutdown command".into(),
        _ => by.to_string(),
    })
}

/// How the session before ended, in a few words.
fn ended_short(b: &Boot) -> (String, Color32) {
    match b.before {
        Shutdown::Crashed(code) => (format!("Stop error 0x{code:X}"), theme::BAD),
        Shutdown::PowerButton => ("Power button held".into(), theme::ELEVATED_TEXT),
        Shutdown::Unexpected => ("Not shut down".into(), theme::ELEVATED_TEXT),
        _ => match (b.ended.action.as_deref(), who(&b.ended)) {
            (Some(action), Some(by)) => {
                let action = match action {
                    "restart" => "Restart",
                    "power off" | "shutdown" => "Shut down",
                    a => a,
                };
                (format!("{action} by {by}"), theme::TEXT_WEAK)
            }
            _ if b.before == Shutdown::Clean => ("Clean".into(), theme::TEXT_WEAK),
            _ => (String::new(), theme::TEXT_WEAK),
        },
    }
}

fn right(ui: &mut Ui, text: impl Into<egui::WidgetText>) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add(Label::new(text).selectable(false));
    });
}

pub fn show(ui: &mut Ui, view: &mut BootsView) {
    view.poll();
    if view.read.is_none() && view.reading.is_none() {
        view.start(ui.ctx());
    }
    let boots = match &view.read {
        None => {
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Reading the system's logs…").color(theme::TEXT_WEAK));
            });
            return;
        }
        Some(Err(e)) => {
            ui.label(RichText::new(format!("The boots could not be read: {e}")).color(theme::BAD));
            return;
        }
        Some(Ok(b)) => b.clone(),
    };
    let unclean = boots.boots.iter().filter(|b| b.before.bad()).count();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "{} boots  ·  {unclean} after a session that did not shut down  ·  read from the \
                 system's logs in {}",
                boots.boots.len(),
                crate::fmt::millis(boots.took)
            ))
            .small()
            .color(theme::TEXT_WEAK),
        );
        if let Some(note) = &boots.note {
            ui.label(RichText::new(note).small().color(theme::ELEVATED_TEXT));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let reading = view.reading.is_some();
            if ui
                .add_enabled(!reading, egui::Button::new("Read again"))
                .clicked()
            {
                view.start(ui.ctx());
            }
            if reading {
                ui.spinner();
            }
        });
    });
    if boots.boots.is_empty() {
        return;
    }
    ui.add_space(4.0);
    summary(ui, &boots.boots[0], &mut view.selected);
    ui.add_space(6.0);
    chart(ui, &boots.boots, &mut view.selected);
    ui.add_space(6.0);

    if let Some(b) = view
        .selected
        .and_then(|at| boots.boots.iter().find(|b| b.started == at))
    {
        let mut close = false;
        egui::Panel::right(egui::Id::new(("boot-pane", b.started)))
            .exact_size(460.0)
            .resizable(false)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        close = pane(
                            ui,
                            b,
                            boots.boots.first().map(|f| f.started) == Some(b.started),
                        )
                    });
            });
        if close {
            view.selected = None;
        }
    }
    egui::CentralPanel::no_frame().show(ui, |ui| {
        table(ui, &boots.boots, &mut view.selected);
    });
}

/// The last boot, in a line of figures.
fn summary(ui: &mut Ui, b: &Boot, selected: &mut Option<i64>) {
    ui.horizontal(|ui| {
        widgets::stat(
            ui,
            140.0,
            "Last boot",
            RichText::new(when(b.started)).color(theme::TEXT),
        );
        let figure = |ms: Option<u64>| match ms {
            Some(ms) => widgets::strong(secs(ms), theme::SUBTITLE),
            None => RichText::new("—").color(theme::TEXT_WEAK),
        };
        widgets::stat(ui, 100.0, "Firmware", figure(b.firmware_ms));
        widgets::stat(ui, 190.0, SYSTEM_PART, figure(system_ms(b)));
        if cfg!(windows) {
            widgets::stat(ui, 150.0, "Sign-in to desktop", figure(b.to_desktop_ms));
            widgets::stat(ui, 120.0, "All told", figure(together(b)));
        }
        let (text, color) = ended_short(b);
        widgets::stat(
            ui,
            220.0,
            "The session before",
            RichText::new(if text.is_empty() { "—".into() } else { text }).color(color),
        );
        if ui.button("Details").clicked() {
            *selected = Some(b.started);
        }
    });
}

/// The steps of a start, as (colour, ms, what).
fn steps(b: &Boot) -> Vec<(Color32, u64, &'static str)> {
    let mut out = Vec::new();
    if let Some(f) = b.firmware_ms {
        out.push((FIRMWARE, f, "Firmware"));
    }
    if let Some(l) = b.loader_ms {
        out.push((LOADER, l, "Loader"));
    }
    match (b.to_screen_ms, b.ready_ms) {
        (Some(s), _) => out.push((SYSTEM, s, SYSTEM_PART)),
        (None, Some(r)) => out.push((SYSTEM, r, SYSTEM_PART)),
        _ => {}
    }
    if let (Some(s), Some(r)) = (b.to_screen_ms, b.ready_ms) {
        out.push((WAIT, r.saturating_sub(s), "At the sign-in screen"));
    }
    if let Some(d) = b.to_desktop_ms {
        out.push((DESKTOP, d, "Sign-in to desktop"));
    }
    out
}

/// A bar for each boot, oldest on the left, its steps stacked; the wait at
/// the sign-in screen is left out. A dot under a boot whose session before
/// did not shut down, red for a stop error.
fn chart(ui: &mut Ui, boots: &[Boot], selected: &mut Option<i64>) {
    let height = 130.0;
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, theme::RADIUS_PANEL, theme::STRIPE);
    let plot = Rect::from_min_max(rect.min + vec2(44.0, 10.0), rect.max - vec2(10.0, 24.0));
    let bar_total = |b: &Boot| -> u64 {
        steps(b)
            .iter()
            .filter(|(c, ..)| *c != WAIT)
            .map(|(_, ms, _)| *ms)
            .sum::<u64>()
            .min(CHART_CAP_MS)
    };
    let most = boots.iter().map(bar_total).max().unwrap_or(1).max(10_000);
    let step = if most > 90_000 { 30_000 } else { 10_000 };
    let y_of = |ms: u64| plot.bottom() - plot.height() * (ms.min(most) as f32 / most as f32);
    let mut tick = step;
    while tick <= most {
        let y = y_of(tick);
        p.hline(plot.x_range(), y, egui::Stroke::new(1.0, theme::GRID));
        p.text(
            egui::pos2(plot.left() - 6.0, y),
            Align2::RIGHT_CENTER,
            format!("{} s", tick / 1000),
            egui::FontId::proportional(theme::CAPTION),
            theme::TEXT_DIM,
        );
        tick += step;
    }
    let n = boots.len();
    let slot = plot.width() / n.max(1) as f32;
    let bar = (slot * 0.7).clamp(3.0, 28.0);
    let pointer = ui.ctx().pointer_hover_pos();
    for (i, b) in boots.iter().rev().enumerate() {
        let cx = plot.left() + slot * (i as f32 + 0.5);
        let column = Rect::from_x_y_ranges(
            egui::Rangef::new(cx - slot / 2.0, cx + slot / 2.0),
            rect.y_range(),
        );
        let hovered = pointer.is_some_and(|p| column.contains(p));
        let is_selected = *selected == Some(b.started);
        if hovered || is_selected {
            p.rect_filled(
                Rect::from_x_y_ranges(column.x_range(), plot.y_range()),
                2.0,
                theme::HOVER,
            );
        }
        let x = egui::Rangef::new(cx - bar / 2.0, cx + bar / 2.0);
        let mut at = 0u64;
        for (color, ms, _) in steps(b).into_iter().filter(|(c, ..)| *c != WAIT) {
            let top = (at + ms).min(CHART_CAP_MS);
            if top > at {
                p.rect_filled(
                    Rect::from_x_y_ranges(x, egui::Rangef::new(y_of(top), y_of(at))),
                    0.0,
                    color,
                );
            }
            at = top;
        }
        if b.before.bad() {
            let color = if matches!(b.before, Shutdown::Crashed(_)) {
                theme::BAD
            } else {
                theme::ELEVATED_TEXT
            };
            p.circle_filled(egui::pos2(cx, plot.bottom() + 8.0), 3.0, color);
        }
        if hovered {
            let response = ui.interact(
                column,
                ui.id().with(("boot bar", b.started)),
                Sense::click(),
            );
            let response = response.on_hover_ui(|ui| {
                ui.label(widgets::strong(when(b.started), theme::BODY));
                for (color, ms, what) in steps(b) {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                        ui.painter().rect_filled(r, 2.0, color);
                        ui.label(format!("{what}  {}", secs(ms)));
                    });
                }
                let (text, _) = ended_short(b);
                if !text.is_empty() {
                    ui.label(format!("The session before: {text}"));
                }
            });
            if response.clicked() {
                *selected = if is_selected { None } else { Some(b.started) };
            }
        }
    }
    let mut x = rect.right() - 10.0;
    for (label, color) in [
        ("sign-in to desktop", DESKTOP),
        (SYSTEM_PART, SYSTEM),
        ("loader", LOADER),
        ("firmware", FIRMWARE),
    ] {
        let galley = p.layout_no_wrap(
            label.to_lowercase(),
            egui::FontId::proportional(theme::CAPTION),
            theme::TEXT_WEAK,
        );
        x -= galley.size().x;
        let y = rect.bottom() - 9.0;
        p.galley(
            egui::pos2(x, y - galley.size().y / 2.0),
            galley,
            theme::TEXT_WEAK,
        );
        x -= 12.0;
        p.rect_filled(
            Rect::from_center_size(egui::pos2(x + 4.0, y), vec2(8.0, 8.0)),
            2.0,
            color,
        );
        x -= 14.0;
    }
}

const TITLES: [(&str, bool); 8] = [
    ("Started", false),
    ("Firmware", true),
    ("Windows", true),
    ("To desktop", true),
    ("All told", true),
    ("The session before", false),
    ("Problems", false),
    ("Session", true),
];
const COLUMNS: [Spec; 8] = [
    Spec::new(124.0),
    Spec::new(72.0),
    Spec::new(72.0),
    Spec::new(80.0),
    Spec::new(76.0),
    Spec::new(220.0).clip(),
    Spec::new(260.0).clip(),
    Spec::new(90.0),
];

fn table(ui: &mut Ui, boots: &[Boot], selected: &mut Option<i64>) {
    let mut pick = None;
    Columns::load(ui.ctx(), "boots-2", &COLUMNS).show(ui, |table, columns| {
        table
            .striped(true)
            .resizable(true)
            .auto_shrink([false, false])
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .header(20.0, |mut h| {
                columns.titles(&mut h, |ui, c| {
                    let (title, numeric) = TITLES[c];
                    let title = if c == 2 && !cfg!(windows) {
                        "System"
                    } else {
                        title
                    };
                    widgets::column_header(ui, title, numeric);
                });
            })
            .body(|body| {
                columns.keep(body.widths());
                body.rows(widgets::ROW, boots.len(), |mut row| {
                    let b = &boots[row.index()];
                    row.set_selected(*selected == Some(b.started));
                    for &c in columns.order() {
                        row.col(|ui| match c {
                            0 => {
                                ui.label(when(b.started));
                            }
                            1 => right(ui, b.firmware_ms.map_or(String::new(), secs)),
                            2 => right(ui, system_ms(b).map_or(String::new(), secs)),
                            3 => right(ui, b.to_desktop_ms.map_or(String::new(), secs)),
                            4 => {
                                if let Some(t) = together(b) {
                                    right(ui, widgets::strong(secs(t), theme::BODY));
                                }
                            }
                            5 => {
                                let (text, color) = ended_short(b);
                                ui.add(Label::new(RichText::new(text).color(color)).truncate());
                            }
                            6 => {
                                let text = match b.problems.as_slice() {
                                    [] => String::new(),
                                    [one] => one.what.clone(),
                                    [first, rest @ ..] => {
                                        format!("{} (and {} more)", first.what, rest.len())
                                    }
                                };
                                ui.add(
                                    Label::new(RichText::new(text).color(theme::TEXT_WEAK))
                                        .truncate(),
                                );
                            }
                            _ => right(
                                ui,
                                RichText::new(b.session.lasted_ms.map_or("running".into(), secs))
                                    .color(theme::TEXT_WEAK),
                            ),
                        });
                    }
                    if row.response().clicked() {
                        pick = Some(b.started);
                    }
                });
            })
    });
    if let Some(at) = pick {
        *selected = if *selected == Some(at) {
            None
        } else {
            Some(at)
        };
    }
}

/// A start's steps side by side, in proportion, with their times.
fn timeline(ui: &mut Ui, b: &Boot) {
    let parts = steps(b);
    // The wait at the sign-in screen is someone's, not the machine's: a
    // short gap in the bar, whatever its length.
    const GAP: f32 = 0.06;
    let waited = parts.iter().any(|(c, ms, _)| *c == WAIT && *ms > 0);
    let total: u64 = parts
        .iter()
        .filter(|(c, ..)| *c != WAIT)
        .map(|(_, ms, _)| *ms)
        .sum::<u64>()
        .max(1);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 14.0), Sense::hover());
    let mut x = rect.left();
    let room = if waited { 1.0 - GAP } else { 1.0 };
    for (color, ms, _) in &parts {
        let share = if *color == WAIT {
            if *ms == 0 { 0.0 } else { GAP }
        } else {
            (*ms as f32 / total as f32 * room).max(0.01)
        };
        let w = (share * width).min(rect.right() - x);
        if w <= 0.0 {
            break;
        }
        ui.painter().rect_filled(
            Rect::from_min_size(egui::pos2(x, rect.top()), vec2(w, rect.height())),
            0.0,
            *color,
        );
        x += w;
    }
    for (color, ms, what) in parts {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, color);
            widgets::text(
                ui,
                230.0,
                Align::Min,
                RichText::new(what).color(theme::TEXT_WEAK),
            );
            ui.label(secs(ms));
        });
    }
}

/// One boot in full; true when it is closed.
fn pane(ui: &mut Ui, b: &Boot, latest: bool) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.label(widgets::strong(when(b.started), theme::SUBTITLE));
        ui.label(RichText::new(b.kind.label()).color(theme::TEXT_WEAK));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            close = ui
                .small_button(widgets::icon(theme::icon::CLOSE, 10.0))
                .on_hover_text("Close")
                .clicked();
        });
    });
    let fact = |ui: &mut Ui, label: &str, value: String| {
        ui.horizontal(|ui| {
            widgets::text(
                ui,
                150.0,
                Align::Min,
                RichText::new(label).color(theme::TEXT_WEAK),
            );
            ui.add(Label::new(value).wrap());
        });
    };
    let note = |ui: &mut Ui, text: &str| {
        ui.label(RichText::new(text).small().color(theme::TEXT_WEAK));
    };

    panes::section(ui, "The start, step by step");
    timeline(ui, b);
    if let Some(t) = together(b) {
        fact(ui, "All told", secs(t));
        note(
            ui,
            "From power on to the desktop, without the time the sign-in screen waited for \
             someone.",
        );
    }
    if let Some(m) = b.measured {
        fact(
            ui,
            "Windows' own measure",
            format!(
                "{} to the desktop, {} after it",
                secs(m.main_path_ms),
                secs(m.after_ms)
            ),
        );
    }

    panes::section(ui, "How the session before ended");
    let e = &b.ended;
    match b.before {
        Shutdown::Crashed(code) => {
            fact(ui, "Stop error", format!("0x{code:X}"));
            note(
                ui,
                "Windows stopped with an error (a blue screen) and restarted.",
            );
        }
        Shutdown::PowerButton => {
            note(
                ui,
                "Someone held the power button: the system was not shut down.",
            );
        }
        Shutdown::Unexpected => {
            note(
                ui,
                "It stopped without shutting down: the power went, it stopped responding, or it \
                 was reset.",
            );
        }
        _ => {}
    }
    if let Some(by) = who(e) {
        let action = match e.action.as_deref() {
            Some("restart") => "A restart",
            Some("power off") | Some("shutdown") => "A shutdown",
            _ => "It",
        };
        let at = e
            .asked_at
            .map_or(String::new(), |t| format!(", at {}", when(t)));
        fact(ui, "Asked for by", format!("{action} by {by}{at}"));
        if let Some(program) = &e.by
            && !by.contains(program.as_str())
        {
            fact(ui, "Program", program.clone());
        }
        if let Some(user) = &e.user {
            fact(ui, "As", user.clone());
        }
        if let Some(reason) = &e.reason {
            fact(ui, "Reason", reason.clone());
        }
        if let Some(comment) = &e.comment {
            fact(ui, "Its words", comment.clone());
        }
        if let Some(ms) = e.shutdown_ms {
            fact(ui, "Shutting down took", secs(ms));
        }
    } else if b.before == Shutdown::Clean {
        note(ui, "It shut down cleanly; the logs do not say who asked.");
    }
    if let Some(t) = e.last_alive {
        fact(ui, "Last seen running", when_exactly(t));
    }
    if let Some(ms) = e.off_ms {
        fact(ui, "Off for", secs(ms));
    }
    if let Some(crash) = &e.crash {
        fact(ui, "The error", crash.clone());
    }
    if let Some(dump) = &e.dump {
        fact(ui, "Its dump", dump.clone());
    }

    if cfg!(windows) {
        panes::section(ui, "The sign-in");
        match b.ready_ms {
            Some(ms) => fact(ui, "Signed in", format!("{} after the start", secs(ms))),
            None => note(ui, "No one signed in in this session."),
        }
        if let Some(ms) = b.policy_start_ms.filter(|&ms| ms > 0) {
            fact(ui, "Group Policy at start", secs(ms));
        }
        if let Some(ms) = b.policy_sign_in_ms.filter(|&ms| ms > 0) {
            fact(ui, "Group Policy at sign-in", secs(ms));
        }
        for (step, ms) in &b.sign_in_steps {
            fact(ui, step, secs(*ms));
        }
    }

    panes::section(ui, "As it started");
    if b.problems.is_empty() {
        note(ui, "No service or driver failed in its first ten minutes.");
    }
    for p in &b.problems {
        ui.horizontal(|ui| {
            widgets::text(
                ui,
                100.0,
                Align::Max,
                RichText::new(format!("+{}", secs(p.after_ms))).color(theme::TEXT_DIM),
            );
            ui.add(Label::new(RichText::new(&p.what).color(theme::ELEVATED_TEXT)).wrap());
        });
    }
    if !b.slowed.is_empty() {
        note(ui, "Slower than usual, as Windows measured it:");
        for s in &b.slowed {
            let more = if s.over_ms > 0 {
                format!("{}, {} more than usual", secs(s.took_ms), secs(s.over_ms))
            } else {
                secs(s.took_ms)
            };
            fact(ui, &format!("{} {}", s.kind.label(), s.name), more);
        }
    }

    panes::section(ui, "The session");
    match b.session.lasted_ms {
        Some(ms) => fact(ui, "It ran for", secs(ms)),
        None if latest => {
            let ran = ((now_filetime() - b.started).max(0) / 10_000) as u64;
            fact(ui, "Running for", secs(ran));
        }
        None => {}
    }
    if b.session.sleeps > 0 {
        fact(ui, "Slept", format!("{} times", b.session.sleeps));
    }
    if b.session.crashed.is_empty() {
        note(ui, "No service stopped unexpectedly after the start.");
    } else {
        note(ui, "Services that stopped unexpectedly:");
        for p in &b.session.crashed {
            ui.horizontal(|ui| {
                widgets::text(
                    ui,
                    100.0,
                    Align::Max,
                    RichText::new(format!("+{}", secs(p.after_ms))).color(theme::TEXT_DIM),
                );
                ui.add(Label::new(&p.what).wrap());
            });
        }
    }
    close
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_times_in_words() {
        assert_eq!(secs(16_225), "16.2 s");
        assert_eq!(secs(72_000), "1 min 12 s");
        assert_eq!(secs(4_177_722), "1 h 9 min");
        assert_eq!(secs(199_075_877), "2 d 7 h");
    }

    #[test]
    fn names_who_asked() {
        let e = |by: &str, user: &str| Ending {
            by: Some(by.into()),
            user: Some(user.into()),
            ..Ending::default()
        };
        assert_eq!(
            who(&e("TrustedInstaller.exe", r"NT AUTHORITY\SYSTEM")).as_deref(),
            Some("Windows Update")
        );
        assert_eq!(
            who(&e("StartMenuExperienceHost.exe", r"PC\Cosmus")).as_deref(),
            Some("you, from the Start menu")
        );
        assert_eq!(
            who(&e("winlogon.exe", r"PC\Cosmus")).as_deref(),
            Some("you")
        );
        assert_eq!(
            who(&e("game.exe", r"PC\Cosmus")).as_deref(),
            Some("game.exe")
        );
    }
}
