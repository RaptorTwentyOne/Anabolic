//! The Stutters page: the programs drawing frames (games) with their frame
//! rate and frame times, every stutter with what else happened at that
//! moment, and the drivers that hold the processors in interrupts and DPCs.
//! All of it from ETW: present events the graphics runtime writes and the
//! kernel's DPC/ISR timing. No game is opened or touched.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anabolic_core::frames::{FrameStats, ProcessFrames, Stutter};
use anabolic_core::latency::{DriverLatency, Kind, LatencySpike, LatencyStats};
use anabolic_core::{ProcState, Snapshot};
use eframe::egui::{
    self, Align, Color32, Label, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, Ui,
    vec2,
};

use crate::proc_table::UiCmd;
use crate::{fmt, theme, widgets};

/// Moments kept to explain stutters with: ten minutes at one a second.
const MOMENTS: usize = 600;
/// Stutters listed, newest first.
const STUTTERS_SHOWN: usize = 40;
/// Drivers listed, worst first.
const DRIVERS_SHOWN: usize = 24;

/// What the machine was doing over one refresh.
struct Moment {
    /// When the refresh was taken; it covers the interval before.
    at: SystemTime,
    interval: Duration,
    cpu: f32,
    disk: f64,
    /// The busiest processes: pid, name, CPU %.
    top: Vec<(u32, Arc<str>, f32)>,
    /// Processes that started during it.
    started: Vec<Arc<str>>,
}

#[derive(Default)]
pub struct StuttersPage {
    /// The program whose frame times are drawn.
    pub selected: Option<u32>,
    moments: VecDeque<Moment>,
    last_seq: u64,
}

impl StuttersPage {
    /// Remembers what each refresh saw while frames are traced, so a
    /// stutter can be told what else ran then.
    pub fn observe(&mut self, snap: &Snapshot) {
        if snap.system.frames.is_none() || snap.seq == self.last_seq {
            return;
        }
        self.last_seq = snap.seq;
        let mut top: Vec<(u32, Arc<str>, f32)> = snap
            .processes
            .iter()
            .filter(|p| p.pid != 0 && p.state != ProcState::Exited && p.cpu >= 0.5)
            .map(|p| (p.pid, p.name.clone(), p.cpu))
            .collect();
        top.sort_by(|a, b| b.2.total_cmp(&a.2));
        top.truncate(4);
        let started = snap
            .processes
            .iter()
            .filter(|p| p.first_seen == snap.seq && snap.seq > 1)
            .map(|p| p.name.clone())
            .take(4)
            .collect();
        self.moments.push_back(Moment {
            at: SystemTime::now(),
            interval: snap.interval,
            cpu: snap.system.cpu.total,
            disk: snap.system.disk_read_rate + snap.system.disk_write_rate,
            top,
            started,
        });
        while self.moments.len() > MOMENTS {
            self.moments.pop_front();
        }
    }

    /// The refresh that covers `at`.
    fn moment(&self, at: SystemTime) -> Option<&Moment> {
        // Present events reach us up to a second late.
        self.moments
            .iter()
            .find(|m| m.at + Duration::from_secs(1) >= at && m.at - m.interval <= at)
    }
}

fn ms(v: f32) -> String {
    if v >= 100.0 {
        format!("{v:.0} ms")
    } else {
        format!("{v:.1} ms")
    }
}

fn micros(us: f32) -> String {
    if us >= 1000.0 {
        format!("{:.2} ms", us / 1000.0)
    } else {
        format!("{us:.0} µs")
    }
}

/// Over a millisecond audio crackles and frames slip; half that is worth a
/// look.
fn latency_color(us: f32) -> Color32 {
    if us >= 1000.0 {
        theme::BAD
    } else if us >= 500.0 {
        theme::ELEVATED_TEXT
    } else {
        theme::TEXT
    }
}

fn caption(ui: &mut Ui, text: impl Into<String>) {
    ui.add(
        Label::new(
            RichText::new(text.into())
                .size(theme::META)
                .color(theme::TEXT_WEAK),
        )
        .wrap(),
    );
}

fn display(p: &ProcessFrames) -> String {
    match &p.window {
        Some(w) => {
            let name = w
                .monitor
                .trim_start_matches(r"\\.\")
                .replace("DISPLAY", "Display ");
            format!(
                "{name}{}  ·  {} Hz{}",
                if w.primary { " (main)" } else { "" },
                w.refresh_hz,
                if w.fullscreen {
                    "  ·  full screen"
                } else {
                    ""
                }
            )
        }
        None => "no window of its own".into(),
    }
}

fn programs(ui: &mut Ui, page: &mut StuttersPage, frames: &FrameStats) {
    widgets::section(ui, "Programs drawing frames");
    if frames.processes.is_empty() {
        caption(
            ui,
            "Nothing is drawing frames steadily right now. Start a game (or anything that \
             animates) and it shows up here within a second or two.",
        );
        return;
    }
    let columns: [(&str, f32, Align); 9] = [
        ("Program", 210.0, Align::Min),
        ("API", 90.0, Align::Min),
        ("FPS", 60.0, Align::Max),
        ("Average", 76.0, Align::Max),
        ("1 % low", 70.0, Align::Max),
        ("99 %", 70.0, Align::Max),
        ("Longest", 76.0, Align::Max),
        ("Stutters", 70.0, Align::Max),
        ("Display", 300.0, Align::Min),
    ];
    widgets::header(ui, &columns);
    for (i, p) in frames.processes.iter().enumerate() {
        let selected = page.selected == Some(p.pid);
        let w = |n: usize| columns[n].1;
        let response = ui
            .scope(|ui| {
                widgets::row(ui, i, |ui| {
                    widgets::cell(ui, w(0), Align::Min, |ui| {
                        let mut text = RichText::new(format!("{}  ({})", p.name, p.pid));
                        if selected {
                            text = text.color(theme::ACCENT);
                        }
                        if p.window.as_ref().is_some_and(|w| w.foreground) {
                            text = text.strong();
                        }
                        ui.add(Label::new(text).truncate());
                    });
                    widgets::text(
                        ui,
                        w(1),
                        Align::Min,
                        RichText::new(p.api.label()).color(theme::TEXT_WEAK),
                    );
                    widgets::text(ui, w(2), Align::Max, format!("{:.0}", p.fps));
                    widgets::text(ui, w(3), Align::Max, ms(p.avg_ms));
                    widgets::text(ui, w(4), Align::Max, format!("{:.0} fps", p.low_fps));
                    widgets::text(ui, w(5), Align::Max, ms(p.p99_ms));
                    widgets::text(ui, w(6), Align::Max, ms(p.max_ms));
                    widgets::text(
                        ui,
                        w(7),
                        Align::Max,
                        RichText::new(if p.stutters == 0 {
                            String::new()
                        } else {
                            p.stutters.to_string()
                        })
                        .color(theme::BAD),
                    );
                    widgets::text(
                        ui,
                        w(8),
                        Align::Min,
                        RichText::new(display(p)).color(theme::TEXT_WEAK),
                    );
                });
            })
            .response
            .interact(Sense::click());
        if response.clicked() {
            page.selected = Some(p.pid);
        }
    }
    let shown = page
        .selected
        .and_then(|pid| frames.processes.iter().find(|p| p.pid == pid))
        .or_else(|| {
            frames
                .processes
                .iter()
                .find(|p| p.window.as_ref().is_some_and(|w| w.foreground))
        })
        .or(frames.processes.first());
    if let Some(p) = shown {
        ui.add_space(8.0);
        frame_graph(ui, p, frames);
    }
}

/// The last frames of one program, one bar each; stutters in red, the
/// monitor's refresh interval as a line.
fn frame_graph(ui: &mut Ui, p: &ProcessFrames, frames: &FrameStats) {
    let refresh = p.window.as_ref().map(|w| w.refresh_hz).filter(|&hz| hz > 0);
    let interval = refresh.map(|hz| 1000.0 / hz as f32);
    caption(
        ui,
        format!(
            "{}: the last {} frames{}  ·  taller is slower; red ones are stutters",
            p.name,
            p.frame_times.len(),
            match (refresh, interval) {
                (Some(hz), Some(i)) =>
                    format!("  ·  its display refreshes every {i:.1} ms ({hz} Hz)"),
                _ => String::new(),
            }
        ),
    );
    let width = ui.available_width().min(1400.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 150.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, theme::BG);
    let times = &p.frame_times;
    // Scale to the slow frames, not the average, so a stutter stands out
    // and does not flatten everything else.
    let raw = (p.p99_ms * 1.6)
        .max(interval.unwrap_or(0.0) * 2.5)
        .max(p.avg_ms * 2.0);
    // Steps of 10 ms up to 100: 1-2-5 steps would leave a 60 fps game
    // using a third of the height.
    let top = if raw <= 100.0 {
        ((raw / 10.0).ceil() * 10.0).max(10.0)
    } else {
        crate::graph::nice_ceiling(raw)
    };
    let grid = Stroke::new(1.0, theme::GRID);
    for i in 1..4 {
        painter.hline(
            rect.x_range(),
            rect.top() + rect.height() * i as f32 / 4.0,
            grid,
        );
    }
    let y = |v: f32| rect.bottom() - (v / top).min(1.0) * rect.height();
    if !times.is_empty() {
        let n = times.len().min(rect.width() as usize).max(1);
        let skip = times.len() - n;
        let dx = rect.width() / n as f32;
        // A stutter is far above its neighbours.
        let stutter_ms: Vec<f32> = frames
            .stutters
            .iter()
            .filter(|s| s.pid == p.pid)
            .map(|s| s.frame_ms)
            .collect();
        for (i, &v) in times.iter().skip(skip).enumerate() {
            let x = rect.left() + i as f32 * dx;
            let late = stutter_ms.iter().any(|&s| (s - v).abs() < 0.05);
            let color = if late {
                theme::BAD
            } else {
                theme::GPU.gamma_multiply(0.85)
            };
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x, y(v)),
                    Pos2::new((x + dx - 0.5).max(x + 1.0), rect.bottom()),
                ),
                0.0,
                color,
            );
        }
    }
    if let Some(i) = interval {
        painter.hline(rect.x_range(), y(i), Stroke::new(1.0, theme::TEXT_DIM));
    }
    painter.text(
        rect.left_top() + vec2(6.0, 4.0),
        egui::Align2::LEFT_TOP,
        format!("{top:.0} ms"),
        egui::FontId::proportional(theme::CAPTION),
        theme::TEXT_DIM,
    );
    painter.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );
    response.on_hover_text(format!(
        "Average {}  ·  99 % of frames within {}  ·  longest {}",
        ms(p.avg_ms),
        ms(p.p99_ms),
        ms(p.max_ms)
    ));
}

/// The DPCs and interrupts that ended while the late frame was due.
fn spikes_during<'a>(s: &Stutter, spikes: &'a [LatencySpike]) -> Vec<&'a LatencySpike> {
    let from = s.at - Duration::from_secs_f32(s.frame_ms / 1000.0 + 0.005);
    let to = s.at + Duration::from_millis(5);
    let mut found: Vec<&LatencySpike> = spikes
        .iter()
        .filter(|x| x.at >= from && x.at <= to)
        .collect();
    found.sort_by(|a, b| b.us.total_cmp(&a.us));
    found.truncate(2);
    found
}

/// What else was going on when a stutter happened, in a line.
fn explain(s: &Stutter, page: &StuttersPage, latency: Option<&LatencyStats>) -> String {
    let mut parts = Vec::new();
    if let Some(l) = latency {
        for x in spikes_during(s, &l.spikes) {
            parts.push(format!(
                "{} {} {} on CPU {}",
                x.driver,
                match x.kind {
                    Kind::Dpc => "DPC",
                    Kind::Isr => "interrupt",
                },
                micros(x.us),
                x.cpu
            ));
        }
    }
    if let Some(m) = page.moment(s.at) {
        if !m.started.is_empty() {
            parts.push(format!(
                "{} started",
                m.started
                    .iter()
                    .map(|n| &**n)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let others: Vec<String> = m
            .top
            .iter()
            .filter(|(pid, _, _)| *pid != s.pid)
            .take(2)
            .map(|(_, name, cpu)| format!("{name} {cpu:.0} %"))
            .collect();
        parts.push(format!(
            "CPU {:.0} %{}",
            m.cpu,
            if others.is_empty() {
                String::new()
            } else {
                format!(" ({})", others.join(", "))
            }
        ));
        if m.disk >= 1024.0 * 1024.0 {
            parts.push(format!("disk {}", fmt::rate_always(m.disk)));
        }
        if m.interval > Duration::from_secs(2) {
            parts.push(format!("over {} s", m.interval.as_secs()));
        }
    }
    parts.join("  ·  ")
}

fn stutters(ui: &mut Ui, page: &StuttersPage, frames: &FrameStats, latency: Option<&LatencyStats>) {
    widgets::section(ui, "Stutters");
    if frames.stutters.is_empty() {
        caption(
            ui,
            "None so far. A stutter is a frame that took more than twice as long as the ones \
             before it, while the pace was steady.",
        );
        return;
    }
    caption(
        ui,
        "Newest first. Beside each, what else happened then: a driver that held a processor \
         over a millisecond while the frame was due, processes that started, and the busiest \
         ones in that refresh.",
    );
    let columns: [(&str, f32, Align); 5] = [
        ("Time", 100.0, Align::Min),
        ("Program", 170.0, Align::Min),
        ("Frame", 70.0, Align::Max),
        ("Typical", 70.0, Align::Max),
        ("At that moment", 700.0, Align::Min),
    ];
    widgets::header(ui, &columns);
    for (i, s) in frames
        .stutters
        .iter()
        .rev()
        .take(STUTTERS_SHOWN)
        .enumerate()
    {
        let why = explain(s, page, latency);
        widgets::row(ui, i, |ui| {
            widgets::text(
                ui,
                columns[0].1,
                Align::Min,
                RichText::new(fmt::clock(s.at, true)).color(theme::TEXT_WEAK),
            );
            widgets::text(ui, columns[1].1, Align::Min, &*s.name);
            widgets::text(
                ui,
                columns[2].1,
                Align::Max,
                RichText::new(ms(s.frame_ms)).color(theme::BAD),
            );
            widgets::text(
                ui,
                columns[3].1,
                Align::Max,
                RichText::new(ms(s.typical_ms)).color(theme::TEXT_WEAK),
            );
            widgets::cell(ui, columns[4].1, Align::Min, |ui| {
                ui.add(Label::new(RichText::new(&why).color(theme::TEXT_WEAK)).truncate())
                    .on_hover_text(&why);
            });
        });
    }
}

/// Five buckets as a stacked bar: green under 0.1 ms to red over 2 ms.
fn histogram(ui: &mut Ui, width: f32, d: &DriverLatency) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 10.0), Sense::hover());
    let buckets: Vec<u64> = (0..5)
        .map(|b| d.dpc.histogram[b] + d.isr.histogram[b])
        .collect();
    let total: u64 = buckets.iter().sum();
    if total == 0 {
        return;
    }
    let colors = [
        theme::GOOD.gamma_multiply(0.7),
        theme::GOOD,
        theme::ELEVATED_TEXT,
        theme::DISK_WRITE,
        theme::BAD,
    ];
    // Rare long ones would vanish at their true share: at least 2 px each,
    // taken from the short ones.
    let mut widths: Vec<f32> = buckets
        .iter()
        .map(|&n| {
            if n == 0 {
                0.0
            } else {
                (rect.width() * n as f32 / total as f32).max(2.0)
            }
        })
        .collect();
    let longer: f32 = widths[1..].iter().sum();
    widths[0] = (rect.width() - longer).max(0.0).min(widths[0]);
    let mut x = rect.left();
    let painter = ui.painter();
    for (b, w) in widths.into_iter().enumerate() {
        if w <= 0.0 {
            continue;
        }
        let right = (x + w).min(rect.right());
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(x, rect.top()), Pos2::new(right, rect.bottom())),
            0.0,
            colors[b],
        );
        x = right;
    }
    response.on_hover_text(format!(
        "Under 0.1 ms: {}  ·  0.1–0.5 ms: {}  ·  0.5–1 ms: {}  ·  1–2 ms: {}  ·  over 2 ms: {}",
        fmt::count(buckets[0]),
        fmt::count(buckets[1]),
        fmt::count(buckets[2]),
        fmt::count(buckets[3]),
        fmt::count(buckets[4]),
    ));
}

fn drivers(ui: &mut Ui, latency: &LatencyStats) {
    widgets::section(ui, "Drivers holding the processors");
    caption(
        ui,
        format!(
            "Interrupts (ISR) and deferred calls (DPC) run ahead of every program, games \
             included; while one runs, its processor does nothing else. Over a millisecond, \
             audio can crackle and frames slip. Measured for {}{}{}.",
            fmt::span(latency.measured),
            if latency.lost_events > 0 {
                format!(", {} events lost", fmt::count(latency.lost_events as u64))
            } else {
                String::new()
            },
            if latency.discarded > 0 {
                format!(
                    "; {} with an impossible start time left out",
                    fmt::count(latency.discarded)
                )
            } else {
                String::new()
            }
        ),
    );
    if latency.unnamed {
        caption(
            ui,
            "Windows hides driver addresses without administrator rights, so the drivers \
             cannot be named.",
        );
    }
    // The longest per processor, as a strip of bars.
    if !latency.per_cpu_highest_us.is_empty() {
        let n = latency.per_cpu_highest_us.len();
        let width = (ui.available_width().min(900.0) / n as f32).clamp(6.0, 24.0);
        let (rect, response) = ui.allocate_exact_size(vec2(width * n as f32, 28.0), Sense::hover());
        let painter = ui.painter();
        let top = 2000.0f32;
        for (i, &us) in latency.per_cpu_highest_us.iter().enumerate() {
            let h = (us / top).min(1.0) * rect.height();
            let x = rect.left() + i as f32 * width;
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x + 1.0, rect.bottom() - h.max(1.0)),
                    Pos2::new(x + width - 1.0, rect.bottom()),
                ),
                1.0,
                if us < 500.0 {
                    theme::GOOD.gamma_multiply(0.7)
                } else {
                    latency_color(us)
                },
            );
        }
        let worst = latency
            .per_cpu_highest_us
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1));
        if let Some((cpu, us)) = worst {
            response.on_hover_text(format!(
                "The longest DPC or interrupt per logical processor (bars up to 2 ms). Worst: \
                 CPU {cpu}, {}.",
                micros(*us)
            ));
        }
        ui.add_space(4.0);
    }
    let columns: [(&str, f32, Align); 8] = [
        ("Driver", 170.0, Align::Min),
        ("Longest", 80.0, Align::Max),
        ("Lately", 80.0, Align::Max),
        ("DPC/s", 70.0, Align::Max),
        ("Interrupts/s", 90.0, Align::Max),
        ("Of a core", 76.0, Align::Max),
        ("How long they take", 180.0, Align::Min),
        ("File", 360.0, Align::Min),
    ];
    widgets::header(ui, &columns);
    let shown = latency.drivers.len().min(DRIVERS_SHOWN);
    for (i, d) in latency.drivers.iter().take(shown).enumerate() {
        let longest = d.highest_us();
        let lately = d.dpc.recent_highest_us.max(d.isr.recent_highest_us);
        widgets::row(ui, i, |ui| {
            widgets::text(ui, columns[0].1, Align::Min, &*d.driver);
            widgets::text(
                ui,
                columns[1].1,
                Align::Max,
                RichText::new(micros(longest)).color(latency_color(longest)),
            );
            widgets::text(
                ui,
                columns[2].1,
                Align::Max,
                RichText::new(if lately > 0.0 {
                    micros(lately)
                } else {
                    String::new()
                })
                .color(latency_color(lately)),
            );
            widgets::text(
                ui,
                columns[3].1,
                Align::Max,
                format!("{:.0}", d.dpc.per_sec),
            );
            widgets::text(
                ui,
                columns[4].1,
                Align::Max,
                format!("{:.0}", d.isr.per_sec),
            );
            widgets::text(
                ui,
                columns[5].1,
                Align::Max,
                format!("{:.2} %", (d.dpc.busy + d.isr.busy) * 100.0),
            );
            widgets::cell(ui, columns[6].1, Align::Min, |ui| {
                histogram(ui, columns[6].1 - 12.0, d)
            });
            widgets::text(
                ui,
                columns[7].1,
                Align::Min,
                RichText::new(&*d.path).small().color(theme::TEXT_DIM),
            );
        });
    }
    // The table keeps its height as drivers come and go.
    widgets::blank_rows(
        ui,
        shown,
        DRIVERS_SHOWN.min(latency.drivers.len().max(8)),
        100.0,
    );
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    page: &mut StuttersPage,
    snap: &Snapshot,
    elevated: bool,
    watching: &mut bool,
    relaunching: bool,
    out: &mut Vec<UiCmd>,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(
                "From the events the graphics runtime and the kernel write; no game is opened \
                 or touched, so it is safe beside anti-cheat.",
            )
            .size(theme::META)
            .color(theme::TEXT_WEAK),
        );
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.checkbox(watching, "Keep watching with this page closed")
                .on_hover_text(
                    "Stutters are caught while you play with Anabolic in the background or \
                     the notification area; look here afterwards. Costs about 0.2 % of one \
                     processor.",
                );
        });
    });
    ui.add_space(6.0);
    let error = snap
        .system
        .frame_trace_error
        .as_deref()
        .or(snap.system.latency_trace_error.as_deref());
    if !elevated {
        ui.horizontal(|ui| {
            ui.label(widgets::icon(theme::icon::LOCK, 13.0).color(theme::ELEVATED_TEXT));
            ui.label(
                RichText::new(
                    "Frame times and driver timing come from kernel tracing, which needs \
                     Anabolic running as administrator.",
                )
                .color(theme::ELEVATED_TEXT),
            );
            if ui
                .add_enabled(!relaunching, egui::Button::new("Restart as admin"))
                .clicked()
            {
                out.push(UiCmd::RelaunchElevated);
            }
        });
        return;
    }
    if let Some(e) = error {
        ui.label(RichText::new(format!("Tracing could not start: {e}")).color(theme::BAD));
    }
    let Some(frames) = snap.system.frames.clone() else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Starting the traces…").color(theme::TEXT_WEAK));
        });
        return;
    };
    let latency = snap.system.latency.clone();
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            programs(ui, page, &frames);
            ui.add_space(14.0);
            stutters(ui, page, &frames, latency.as_deref());
            ui.add_space(14.0);
            match &latency {
                Some(l) => drivers(ui, l),
                None => caption(ui, "Driver timing is starting…"),
            }
            ui.add_space(10.0);
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn spikes_line_up_with_the_late_frame() {
        let at = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let s = Stutter {
            pid: 1,
            name: "game.exe".into(),
            at,
            frame_ms: 40.0,
            typical_ms: 7.0,
        };
        let spike = |ms_before: u64, us: f32| LatencySpike {
            at: at - Duration::from_millis(ms_before),
            driver: "x.sys".into(),
            kind: Kind::Dpc,
            cpu: 0,
            us,
        };
        let spikes = [spike(30, 1500.0), spike(200, 3000.0), spike(10, 1100.0)];
        let found = spikes_during(&s, &spikes);
        // The one 200 ms earlier ended before the frame was due.
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].us, 1500.0);
    }

    #[test]
    fn clock_has_milliseconds() {
        let t = fmt::clock(UNIX_EPOCH + Duration::from_millis(1_700_000_000_123), true);
        assert!(t.ends_with(".123"), "{t}");
        assert_eq!(t.len(), 12);
    }
}
