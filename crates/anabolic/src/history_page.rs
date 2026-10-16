//! History: what the flight recorder kept. A strip of the day picks an
//! hour, the hour's graph picks a range (drag; a click picks one ten-second
//! record), and the tables say which processes led in that range.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anabolic_core::recorder::{self, BUCKET_SECS, Bucket, Day, Event};
use anabolic_core::{ProcState, Snapshot};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, vec2,
};

use crate::proc_table::UiCmd;
use crate::{fmt, theme, widgets};

const DAY: u32 = 86_400;
const HOUR: u32 = 3_600;
/// Rows per table: fixed, so the page keeps its height.
const ROWS: usize = 8;
/// While today is on screen, newer records are picked up this often.
const RELOAD: Duration = Duration::from_secs(30);

struct Loaded {
    date: String,
    buckets: Vec<Bucket>,
    /// Each bucket's start, seconds since local midnight.
    local: Vec<u32>,
}

struct Answer {
    days: Vec<Day>,
    /// `Ok(None)`: nothing recorded yet.
    loaded: Result<Option<Loaded>, String>,
}

/// One process' total over the selected range.
struct Lead {
    pid: u32,
    name: Arc<str>,
    value: f64,
    peak: f64,
}

#[derive(Default)]
struct Summary {
    /// Seconds of the range that have records.
    secs: f32,
    cpu_avg: f32,
    cpu_max: f32,
    memory_max: u64,
    disk_read: u64,
    disk_write: u64,
    net: u64,
    cpu: Vec<Lead>,
    memory: Vec<Lead>,
    disk: Vec<Lead>,
    /// Disk counters need elevation; without them, all I/O stands in.
    disk_is_io: bool,
    net_leads: Vec<Lead>,
    gpu: Vec<Lead>,
    /// (local seconds, started, event), newest first.
    events: Vec<(u32, bool, Event)>,
    /// Processes that started or exited in the range.
    event_count: u32,
    /// (from, to, pid, name): spans in which a window of the process was
    /// not responding, newest first.
    hangs: Vec<(u32, u32, u32, Arc<str>)>,
}

#[derive(Default)]
pub struct HistoryPage {
    days: Vec<Day>,
    loaded: Option<Loaded>,
    error: Option<String>,
    pending: Option<Receiver<Answer>>,
    loaded_at: Option<Instant>,
    /// The day the user picked; None follows the newest.
    want_day: Option<String>,
    /// First second of the hour on display, local.
    window: Option<u32>,
    /// Selected range in the hour, local seconds, end exclusive.
    selection: Option<(u32, u32)>,
    drag_from: Option<u32>,
    /// (date, from, to, buckets) the summary was made for.
    summary: Option<((String, u32, u32, usize), Summary)>,
}

impl HistoryPage {
    /// Reads the day list and a day on a thread of its own.
    fn request(&mut self, ctx: &egui::Context, dir: PathBuf) {
        let want = self.want_day.clone();
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("anabolic-history".into())
            .spawn(move || {
                let days = recorder::days(&dir);
                let date = want
                    .filter(|d| days.iter().any(|day| &day.date == d))
                    .or_else(|| days.first().map(|d| d.date.clone()));
                let loaded = match date {
                    None => Ok(None),
                    Some(date) => recorder::load(&dir, &date).map(|buckets| {
                        let local = buckets
                            .iter()
                            .map(|b| recorder::local_day(b.start).1)
                            .collect();
                        Some(Loaded {
                            date,
                            buckets,
                            local,
                        })
                    }),
                };
                let _ = tx.send(Answer { days, loaded });
                crate::stalls::wake(&ctx);
            });
        if spawned.is_ok() {
            self.pending = Some(rx);
        }
    }

    fn receive(&mut self) {
        let Some(answer) = self.pending.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            return;
        };
        self.pending = None;
        self.loaded_at = Some(Instant::now());
        self.days = answer.days;
        self.summary = None;
        match answer.loaded {
            Ok(Some(loaded)) => {
                if self.loaded.as_ref().is_none_or(|l| l.date != loaded.date) {
                    // Another day: start at its last recorded hour.
                    self.window = None;
                    self.selection = None;
                }
                self.loaded = Some(loaded);
                self.error = None;
            }
            Ok(None) => {
                self.loaded = None;
                self.error = None;
            }
            Err(e) => {
                self.loaded = None;
                self.error = Some(e);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    page: &mut HistoryPage,
    snap: &Snapshot,
    dir: Option<PathBuf>,
    recording: bool,
    keep_days: u32,
    in_tray: bool,
    out: &mut Vec<UiCmd>,
) {
    page.receive();
    let today = recorder::local_day(recorder::unix_now()).0;
    let showing_today = page.loaded.as_ref().is_none_or(|l| l.date == today);
    let due = page
        .loaded_at
        .is_none_or(|t| showing_today && t.elapsed() > RELOAD);
    if page.pending.is_none()
        && due
        && let Some(dir) = dir
    {
        page.request(ui.ctx(), dir);
    }

    header(ui, page, &today, recording, keep_days, in_tray, out);
    ui.add_space(6.0);

    let Some(loaded) = &page.loaded else {
        let text = if let Some(e) = &page.error {
            format!("The history could not be read: {e}")
        } else if page.loaded_at.is_none() {
            "Reading the history…".to_string()
        } else if recording {
            "Nothing recorded yet. The first record is written ten seconds after Anabolic starts."
                .to_string()
        } else {
            "The history is off. Turn it on in Settings to keep a record of what the machine does."
                .to_string()
        };
        ui.add_space(30.0);
        ui.label(RichText::new(text).color(theme::TEXT_WEAK));
        return;
    };

    // At first, the hour that ends with the latest record.
    let last = loaded.local.last().copied().unwrap_or(0);
    let end = ((last + BUCKET_SECS as u32).div_ceil(600) * 600).clamp(HOUR, DAY);
    let window = *page.window.get_or_insert(end - HOUR);
    let mut window_now = window;
    day_strip(ui, loaded, &mut window_now);
    if window_now != window {
        page.window = Some(window_now);
        page.selection = None;
    }
    ui.add_space(10.0);
    let window = window_now;
    hour_graph(ui, loaded, window, &mut page.selection, &mut page.drag_from);
    ui.add_space(8.0);

    let (from, to) = page.selection.unwrap_or((window, window + HOUR));
    let key = (loaded.date.clone(), from, to, loaded.buckets.len());
    if page.summary.as_ref().is_none_or(|(k, _)| *k != key) {
        page.summary = Some((key, summarize(loaded, from, to)));
    }
    let Some((_, summary)) = &page.summary else {
        return;
    };
    range_line(ui, &mut page.selection, from, to, summary);
    ui.add_space(6.0);
    if summary.secs <= 0.0 {
        ui.label(
            RichText::new("Anabolic was not running then: there is no record of this range.")
                .color(theme::TEXT_WEAK),
        );
        return;
    }
    tables(ui, summary, snap, out);
}

fn header(
    ui: &mut Ui,
    page: &mut HistoryPage,
    today: &str,
    recording: bool,
    keep_days: u32,
    in_tray: bool,
    out: &mut Vec<UiCmd>,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("History").font(theme::brand(theme::TITLE)));
        ui.add_space(12.0);
        let yesterday = recorder::local_day(recorder::unix_now().saturating_sub(86_400)).0;
        let label = |date: &str| match date {
            d if d == today => "Today".to_string(),
            d if d == yesterday => "Yesterday".to_string(),
            d => d.to_string(),
        };
        let current = page.loaded.as_ref().map(|l| l.date.clone());
        egui::ComboBox::from_id_salt("history-day")
            .selected_text(current.as_deref().map_or("No records".into(), label))
            .width(130.0)
            .show_ui(ui, |ui| {
                for d in &page.days {
                    let text = format!("{}  ·  {}", label(&d.date), fmt::bytes(d.bytes));
                    if ui
                        .selectable_label(current.as_deref() == Some(d.date.as_str()), text)
                        .clicked()
                    {
                        page.want_day = Some(d.date.clone());
                        // Read it now.
                        page.loaded_at = None;
                    }
                }
            });
        if page.pending.is_some() {
            ui.spinner();
        }
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            let size: u64 = page.days.iter().map(|d| d.bytes).sum();
            if recording {
                ui.label(
                    RichText::new(format!(
                        "Recorded every {BUCKET_SECS} s while Anabolic runs · kept {keep_days} days · {}",
                        fmt::bytes(size)
                    ))
                    .size(theme::META)
                    .color(theme::TEXT_WEAK),
                );
                // Right to left: this lands before the text.
                if !in_tray
                    && ui
                        .small_button("Keep recording with the window closed")
                        .on_hover_text(
                            "Closing the window then leaves Anabolic in the notification area, \
                             recording; its icon opens it again or quits.",
                        )
                        .clicked()
                {
                    out.push(UiCmd::CloseToTray(true));
                }
            } else {
                ui.label(
                    RichText::new("Recording is off (Settings)")
                        .size(theme::META)
                        .color(theme::ELEVATED_TEXT),
                );
            }
        });
    });
}

fn x_at(rect: Rect, from: u32, span: u32, t: u32) -> f32 {
    rect.left() + (t.saturating_sub(from) as f32 / span as f32) * rect.width()
}

fn t_at(rect: Rect, from: u32, span: u32, x: f32) -> u32 {
    let f = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    from + (f * span as f32) as u32
}

fn hms(t: u32) -> String {
    format!("{:02}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
}

fn hm(t: u32) -> String {
    format!("{:02}:{:02}", t / 3600 % 24, t / 60 % 60)
}

fn memory_pct(b: &Bucket) -> f32 {
    if b.memory_total == 0 {
        0.0
    } else {
        b.memory_used as f32 / b.memory_total as f32 * 100.0
    }
}

/// The whole day: CPU per pixel, the hour on display outlined. Click or
/// drag to move the hour.
fn day_strip(ui: &mut Ui, l: &Loaded, window: &mut u32) {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect.expand(1.0));
    painter.rect_filled(rect, 2.0, theme::BG);
    for h in (3..24).step_by(3) {
        let x = painter.round_to_pixel_center(x_at(rect, 0, DAY, h * HOUR));
        painter.vline(x, rect.y_range(), Stroke::new(1.0, theme::GRID));
    }
    // Highest CPU per pixel column; columns with a record get a baseline
    // so unrecorded time reads as empty, not as idle.
    let width = rect.width().max(1.0) as usize;
    let mut columns = vec![None::<f32>; width];
    for (b, &t) in l.buckets.iter().zip(&l.local) {
        let x = ((t as f32 / DAY as f32) * width as f32) as usize;
        let c = &mut columns[x.min(width - 1)];
        *c = Some(c.unwrap_or(0.0).max(b.cpu_avg));
    }
    let base = rect.bottom() - 1.0;
    let cpu = theme::CPU.gamma_multiply(0.75);
    for (x, c) in columns.iter().enumerate() {
        let Some(cpu_pct) = c else { continue };
        let px = rect.left() + x as f32 + 0.5;
        let top = base - (cpu_pct / 100.0).clamp(0.0, 1.0) * (rect.height() - 4.0);
        painter.vline(
            px,
            (base - 1.0)..=base,
            Stroke::new(1.0, theme::BORDER_STRONG),
        );
        if top < base - 1.0 {
            painter.vline(px, top..=base, Stroke::new(1.0, cpu));
        }
    }
    // Something was not responding: a red mark along the top.
    let mut hangs = false;
    for (b, &t) in l.buckets.iter().zip(&l.local) {
        if !b.hung.is_empty() {
            hangs = true;
            let x0 = x_at(rect, 0, DAY, t);
            let x1 = x_at(rect, 0, DAY, t + BUCKET_SECS as u32).max(x0 + 1.0);
            painter.rect_filled(
                Rect::from_x_y_ranges(x0..=x1, rect.top()..=rect.top() + 3.0),
                0.0,
                theme::BAD,
            );
        }
    }
    let shown = Rect::from_x_y_ranges(
        x_at(rect, 0, DAY, *window)..=x_at(rect, 0, DAY, *window + HOUR),
        rect.y_range(),
    );
    painter.rect_filled(shown, 2.0, theme::SELECTION.gamma_multiply(0.5));
    painter.rect_stroke(
        shown,
        2.0,
        Stroke::new(1.0, theme::TEXT_WEAK),
        egui::StrokeKind::Inside,
    );
    if let Some(pos) = response.interact_pointer_pos()
        && (response.clicked() || response.dragged())
    {
        let t = t_at(rect, 0, DAY, pos.x);
        *window = t.saturating_sub(HOUR / 2).min(DAY - HOUR) / 600 * 600;
    }
    if let Some(pos) = response.hover_pos() {
        let mut text = format!("{}: show this hour", hm(t_at(rect, 0, DAY, pos.x)));
        if hangs {
            text.push_str("\nRed marks: a program was not responding");
        }
        response.on_hover_text(text);
    }
    // Hour marks under the strip.
    let (labels, _) = ui.allocate_exact_size(vec2(rect.width(), 14.0), Sense::hover());
    for h in (0..24).step_by(3) {
        ui.painter().text(
            Pos2::new(x_at(labels, 0, DAY, h * HOUR), labels.top()),
            if h == 0 {
                Align2::LEFT_TOP
            } else {
                Align2::CENTER_TOP
            },
            format!("{h:02}:00"),
            FontId::proportional(theme::CAPTION),
            theme::TEXT_DIM,
        );
    }
}

/// Lanes of the hour view, top to bottom.
#[derive(Clone, Copy, PartialEq)]
enum Lane {
    Cpu,
    Memory,
    Disk,
    Network,
    Gpu,
    Processes,
    Hung,
}

const LANES: [(Lane, f32); 7] = [
    (Lane::Cpu, 96.0),
    (Lane::Memory, 48.0),
    (Lane::Disk, 52.0),
    (Lane::Network, 52.0),
    (Lane::Gpu, 36.0),
    (Lane::Processes, 44.0),
    (Lane::Hung, 16.0),
];
const LANE_GAP: f32 = 6.0;
/// Room left of the lanes for their names and scales.
const GUTTER: f32 = 104.0;
const MIB: f32 = 1024.0 * 1024.0;

/// Bytes per second over a record.
fn per_sec(b: &Bucket, bytes: u64) -> f32 {
    bytes as f32 / b.secs.max(1.0)
}

/// The top of a byte-rate lane: a power of two of MB/s, at least 1 MB/s,
/// so its label stays round and an idle hour does not magnify noise.
fn rate_scale(max: f32) -> f32 {
    ((max / MIB).ceil().max(1.0) as u32).next_power_of_two() as f32 * MIB
}

fn rate_label(bytes_per_sec: f32) -> String {
    let mb = bytes_per_sec / MIB;
    if mb >= 1024.0 {
        format!("{:.0} GB/s", mb / 1024.0)
    } else {
        format!("{mb:.0} MB/s")
    }
}

/// A lane's name, scale and colour key, left of it.
fn lane_label(painter: &egui::Painter, x: f32, lane: Rect, lines: &[&[(&str, Color32)]]) {
    let mut y = lane.top();
    for (i, parts) in lines.iter().enumerate() {
        let size = if i == 0 { theme::META } else { theme::CAPTION };
        let mut job = egui::text::LayoutJob::default();
        for (text, color) in *parts {
            job.append(
                text,
                0.0,
                egui::TextFormat::simple(FontId::proportional(size), *color),
            );
        }
        let galley = painter.layout_job(job);
        let height = galley.size().y;
        painter.galley(Pos2::new(x, y), galley, theme::TEXT_WEAK);
        y += height;
    }
}

/// One hour, a bar per ten-second record, in lanes that share the time
/// axis: CPU (average, with the peak fainter above it), memory, disk and
/// network (in above the line, out below it), GPU, processes started and
/// exited, and hangs. Drag to pick a range, click to pick a record,
/// double-click to clear.
fn hour_graph(
    ui: &mut Ui,
    l: &Loaded,
    window: u32,
    selection: &mut Option<(u32, u32)>,
    drag_from: &mut Option<u32>,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(
                "One bar per ten seconds · disk, network and processes on a square-root scale, so small activity shows · hover a bar for details",
            )
                .size(theme::META)
                .color(theme::TEXT_WEAK),
        );
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new("Drag to pick a range, double-click to clear")
                    .size(theme::META)
                    .color(theme::TEXT_DIM),
            );
        });
    });
    ui.add_space(4.0);

    let first = l.local.partition_point(|&t| t < window);
    let end = l.local.partition_point(|&t| t < window + HOUR);
    let buckets = &l.buckets[first..end];
    let times = &l.local[first..end];
    let peak_of = |f: &dyn Fn(&Bucket) -> f32| buckets.iter().map(f).fold(0.0, f32::max);
    let disk_scale = rate_scale(peak_of(&|b| per_sec(b, b.disk_read.max(b.disk_write))));
    let net_scale = rate_scale(peak_of(&|b| per_sec(b, b.net_rx.max(b.net_tx))));
    let memory_total = buckets.iter().map(|b| b.memory_total).max().unwrap_or(0);
    let count = |events: &[Event]| events.iter().map(|e| e.count).sum::<u32>() as f32;
    let churn_scale = peak_of(&|b| count(&b.started).max(count(&b.exited))).max(1.0);

    let height = LANES.iter().map(|(_, h)| h).sum::<f32>() + LANE_GAP * (LANES.len() - 1) as f32;
    let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let plot = Rect::from_x_y_ranges((outer.left() + GUTTER)..=outer.right(), outer.y_range());
    let response = ui.interact(plot, ui.id().with("history-hour"), Sense::click_and_drag());
    let painter = ui.painter_at(outer.expand(1.0));
    let mut lanes = [(Lane::Cpu, plot); LANES.len()];
    let mut top = outer.top();
    for (slot, (lane, h)) in lanes.iter_mut().zip(LANES) {
        *slot = (lane, Rect::from_x_y_ranges(plot.x_range(), top..=top + h));
        top += h + LANE_GAP;
    }
    let rect_of = |which: Lane| {
        lanes
            .iter()
            .find(|(lane, _)| *lane == which)
            .map_or(plot, |(_, r)| *r)
    };

    let grid = Stroke::new(1.0, theme::GRID);
    let peak_color = theme::CPU.gamma_multiply(0.22);
    let total = fmt::bytes(memory_total);
    let (disk_top, net_top) = (rate_label(disk_scale), rate_label(net_scale));
    let churn_top = format!("{churn_scale:.0} per bar");
    for (lane, r) in lanes {
        painter.rect_filled(r, 2.0, theme::BG);
        for m in (10..60).step_by(10) {
            let x = painter.round_to_pixel_center(x_at(r, window, HOUR, window + m * 60));
            painter.vline(x, r.y_range(), grid);
        }
        let x = outer.left();
        let dim = theme::TEXT_DIM;
        match lane {
            Lane::Cpu => {
                for pct in [25.0, 50.0, 75.0] {
                    let y = painter.round_to_pixel_center(r.bottom() - r.height() * pct / 100.0);
                    painter.hline(r.x_range(), y, grid);
                }
                lane_label(
                    &painter,
                    x,
                    r,
                    &[
                        &[("CPU", theme::TEXT_WEAK)],
                        &[("0 – 100 %", dim)],
                        &[("average", theme::CPU)],
                        &[("peak", theme::CPU.gamma_multiply(0.6))],
                    ],
                );
            }
            Lane::Memory => lane_label(
                &painter,
                x,
                r,
                &[
                    &[("Memory", theme::TEXT_WEAK)],
                    &[("in use, of ", dim), (&total, dim)],
                ],
            ),
            Lane::Disk | Lane::Network => {
                let y = painter.round_to_pixel_center(r.center().y);
                painter.hline(r.x_range(), y, Stroke::new(1.0, theme::BORDER));
                let (name, top, up, down) = if lane == Lane::Disk {
                    (
                        "Disk",
                        &disk_top,
                        ("↑ read  ", theme::DISK_READ),
                        ("↓ written", theme::DISK_WRITE),
                    )
                } else {
                    (
                        "Network",
                        &net_top,
                        ("↑ in  ", theme::NET_RX),
                        ("↓ out", theme::NET_TX),
                    )
                };
                lane_label(
                    &painter,
                    x,
                    r,
                    &[
                        &[(name, theme::TEXT_WEAK)],
                        &[("up to ", dim), (top, dim)],
                        &[up, down],
                    ],
                );
            }
            Lane::Gpu => lane_label(
                &painter,
                x,
                r,
                &[&[("GPU", theme::TEXT_WEAK)], &[("busiest, peak", dim)]],
            ),
            Lane::Processes => {
                let y = painter.round_to_pixel_center(r.center().y);
                painter.hline(r.x_range(), y, Stroke::new(1.0, theme::BORDER));
                lane_label(
                    &painter,
                    x,
                    r,
                    &[
                        &[("Processes", theme::TEXT_WEAK)],
                        &[("up to ", dim), (&churn_top, dim)],
                        &[("↑ started  ", theme::GOOD), ("↓ exited", theme::TEXT_WEAK)],
                    ],
                );
            }
            Lane::Hung => lane_label(&painter, x, r, &[&[("Not responding", theme::BAD)]]),
        }
    }

    let step = plot.width() * BUCKET_SECS as f32 / HOUR as f32;
    let fraction = |r: Rect, f: f32| r.bottom() - f.clamp(0.0, 1.0) * r.height();
    let [cpu, mem, disk, net, gpu, churn, hung] = [
        Lane::Cpu,
        Lane::Memory,
        Lane::Disk,
        Lane::Network,
        Lane::Gpu,
        Lane::Processes,
        Lane::Hung,
    ]
    .map(rect_of);
    // In above a lane's middle line, out below it; square-root heights.
    let mirrored = |r: Rect,
                    xs: std::ops::RangeInclusive<f32>,
                    up: f32,
                    down: f32,
                    colors: (Color32, Color32)| {
        let (mid, half) = (r.center().y, r.height() / 2.0 - 1.0);
        let height = |f: f32| f.clamp(0.0, 1.0).sqrt() * half;
        let (up, down) = (height(up), height(down));
        if up >= 0.5 {
            painter.rect_filled(
                Rect::from_x_y_ranges(xs.clone(), (mid - up)..=mid),
                0.0,
                colors.0,
            );
        }
        if down >= 0.5 {
            painter.rect_filled(Rect::from_x_y_ranges(xs, mid..=(mid + down)), 0.0, colors.1);
        }
    };
    let mut memory: Vec<Pos2> = Vec::new();
    let mut lines: Vec<Vec<Pos2>> = Vec::new();
    let mut previous: Option<u32> = None;
    for (b, &t) in buckets.iter().zip(times) {
        let x0 = x_at(plot, window, HOUR, t);
        let x1 = (x0 + step - 0.5).max(x0 + 1.0);
        let xs = x0..=x1;
        let bar = |r: Rect, y: f32| Rect::from_x_y_ranges(xs.clone(), y..=r.bottom());

        let (avg, peak) = (
            fraction(cpu, b.cpu_avg / 100.0),
            fraction(cpu, b.cpu_max / 100.0),
        );
        if peak < avg {
            painter.rect_filled(
                Rect::from_x_y_ranges(xs.clone(), peak..=avg),
                0.0,
                peak_color,
            );
        }
        painter.rect_filled(bar(cpu, avg), 0.0, theme::CPU.gamma_multiply(0.6));

        // The memory line breaks where nothing was recorded.
        if previous.is_some_and(|p| t > p + BUCKET_SECS as u32) {
            lines.push(std::mem::take(&mut memory));
        }
        memory.push(Pos2::new(
            x0 + step / 2.0,
            fraction(mem, memory_pct(b) / 100.0),
        ));
        previous = Some(t);

        mirrored(
            disk,
            xs.clone(),
            per_sec(b, b.disk_read) / disk_scale,
            per_sec(b, b.disk_write) / disk_scale,
            (
                theme::DISK_READ.gamma_multiply(0.75),
                theme::DISK_WRITE.gamma_multiply(0.75),
            ),
        );
        mirrored(
            net,
            xs.clone(),
            per_sec(b, b.net_rx) / net_scale,
            per_sec(b, b.net_tx) / net_scale,
            (
                theme::NET_RX.gamma_multiply(0.75),
                theme::NET_TX.gamma_multiply(0.75),
            ),
        );
        if b.gpu_max >= 1.0 {
            painter.rect_filled(
                bar(gpu, fraction(gpu, b.gpu_max / 100.0)),
                0.0,
                theme::GPU.gamma_multiply(0.6),
            );
        }

        mirrored(
            churn,
            xs.clone(),
            count(&b.started) / churn_scale,
            count(&b.exited) / churn_scale,
            (theme::GOOD.gamma_multiply(0.7), theme::TEXT_DIM),
        );
        if !b.hung.is_empty() {
            painter.rect_filled(
                Rect::from_x_y_ranges(xs.clone(), hung.shrink2(vec2(0.0, 2.0)).y_range()),
                0.0,
                theme::BAD,
            );
        }
    }
    lines.push(memory);
    for line in lines.into_iter().filter(|l| l.len() > 1) {
        painter.add(crate::graph::area_mesh(
            &line,
            mem.bottom(),
            theme::MEMORY.gamma_multiply(0.18),
        ));
        painter.add(Shape::line(line, Stroke::new(1.5, theme::MEMORY)));
    }

    // Picking: snapped to records.
    let snap_t = |x: f32| t_at(plot, window, HOUR, x) / BUCKET_SECS as u32 * BUCKET_SECS as u32;
    if response.double_clicked() {
        *selection = None;
    } else if let Some(pos) = response.interact_pointer_pos() {
        let t = snap_t(pos.x);
        if response.drag_started() {
            *drag_from = Some(t);
        }
        if response.dragged()
            && let Some(start) = *drag_from
        {
            let (a, b) = (start.min(t), start.max(t));
            *selection = Some((a, b + BUCKET_SECS as u32));
        } else if response.clicked() {
            *selection = Some((t, t + BUCKET_SECS as u32));
        }
    }
    if response.drag_stopped() {
        *drag_from = None;
    }
    if let Some((a, b)) = *selection {
        let picked = Rect::from_x_y_ranges(
            x_at(plot, window, HOUR, a)..=x_at(plot, window, HOUR, b),
            plot.y_range(),
        );
        painter.rect_filled(picked, 0.0, theme::SELECTION.gamma_multiply(0.45));
        painter.rect_stroke(
            picked,
            0.0,
            Stroke::new(1.0, theme::TEXT_WEAK),
            egui::StrokeKind::Inside,
        );
    }

    if let Some(pos) = response.hover_pos() {
        let t = snap_t(pos.x);
        painter.vline(
            pos.x,
            plot.y_range(),
            Stroke::new(1.0, theme::BORDER_STRONG),
        );
        let hovered = times.binary_search(&t).ok();
        response.on_hover_ui_at_pointer(|ui| match hovered {
            Some(i) => bucket_tooltip(ui, &buckets[i], times[i]),
            None => {
                ui.label(format!("{}: not recorded", hms(t)));
            }
        });
    }

    let (labels, _) = ui.allocate_exact_size(vec2(outer.width(), 14.0), Sense::hover());
    let labels = Rect::from_x_y_ranges(plot.x_range(), labels.y_range());
    for m in (0..=60).step_by(10) {
        ui.painter().text(
            Pos2::new(x_at(labels, window, HOUR, window + m * 60), labels.top()),
            match m {
                0 => Align2::LEFT_TOP,
                60 => Align2::RIGHT_TOP,
                _ => Align2::CENTER_TOP,
            },
            hm(window + m * 60),
            FontId::proportional(theme::CAPTION),
            theme::TEXT_DIM,
        );
    }
}

/// Up to four names, then how many more.
fn names(events: &[Event]) -> String {
    let mut text = events
        .iter()
        .take(4)
        .map(|e| {
            if e.count > 1 {
                format!("{} ×{}", e.name, e.count)
            } else {
                e.name.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    if events.len() > 4 {
        text.push_str(&format!(" and {} more", events.len() - 4));
    }
    text
}

fn bucket_tooltip(ui: &mut Ui, b: &Bucket, t: u32) {
    ui.set_max_width(380.0);
    ui.label(widgets::strong(
        format!("{} – {}", hms(t), hms(t + BUCKET_SECS as u32)),
        theme::BODY,
    ));
    let rate = |bytes: u64| fmt::rate_always(per_sec(b, bytes) as f64);
    egui::Grid::new("history-bucket")
        .num_columns(2)
        .spacing([12.0, 2.0])
        .show(ui, |ui| {
            let mut row = |name: &str, color: Color32, value: String| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                    ui.painter().rect_filled(r, 2.0, color);
                    ui.label(RichText::new(name).color(theme::TEXT_WEAK));
                });
                ui.label(value);
                ui.end_row();
            };
            row(
                "CPU",
                theme::CPU,
                format!("{:.0} % average, {:.0} % peak", b.cpu_avg, b.cpu_max),
            );
            row(
                "Memory",
                theme::MEMORY,
                format!(
                    "{} of {} ({:.0} %)",
                    fmt::bytes(b.memory_used),
                    fmt::bytes(b.memory_total),
                    memory_pct(b)
                ),
            );
            let busiest = if b.disk_active > 0.0 {
                format!(" · busiest disk {:.0} % active", b.disk_active)
            } else {
                String::new()
            };
            row(
                "Disk",
                theme::DISK_READ,
                format!(
                    "{} read, {} written{busiest}",
                    rate(b.disk_read),
                    rate(b.disk_write)
                ),
            );
            row(
                "Network",
                theme::NET_RX,
                format!("{} in, {} out", rate(b.net_rx), rate(b.net_tx)),
            );
            if b.gpu_max > 0.0 {
                row("GPU", theme::GPU, format!("{:.0} % peak", b.gpu_max));
            }
        });
    let mut busiest: Vec<_> = b.processes.iter().filter(|p| p.cpu_avg >= 0.1).collect();
    busiest.sort_by(|a, b| b.cpu_avg.total_cmp(&a.cpu_avg));
    if !busiest.is_empty() {
        ui.add_space(4.0);
        ui.label(widgets::strong("Busiest", theme::META).color(theme::TEXT_WEAK));
        for p in busiest.iter().take(3) {
            ui.label(format!(
                "{}  ·  {:.1} % CPU  ·  {} private",
                p.name,
                p.cpu_avg,
                fmt::bytes(p.private)
            ));
        }
    }
    if !b.hung.is_empty() {
        ui.add_space(4.0);
        ui.label(RichText::new(format!("Not responding: {}", names(&b.hung))).color(theme::BAD));
    }
    if !b.started.is_empty() {
        ui.label(RichText::new(format!("Started: {}", names(&b.started))).color(theme::GOOD));
    }
    if !b.exited.is_empty() {
        ui.label(RichText::new(format!("Exited: {}", names(&b.exited))).color(theme::TEXT_WEAK));
    }
}

fn range_line(ui: &mut Ui, selection: &mut Option<(u32, u32)>, from: u32, to: u32, s: &Summary) {
    ui.horizontal(|ui| {
        let span = to - from;
        let length = if span >= 120 {
            format!("{} min", span / 60)
        } else {
            format!("{span} s")
        };
        ui.label(widgets::strong(
            format!("{} – {}  ·  {length}", hms(from), hms(to)),
            theme::BODY,
        ));
        if selection.is_some() {
            if ui.small_button("Whole hour").clicked() {
                *selection = None;
            }
        } else {
            ui.label(
                RichText::new("the whole hour")
                    .size(theme::META)
                    .color(theme::TEXT_WEAK),
            );
        }
        if s.secs > 0.0 {
            ui.add_space(12.0);
            ui.label(
                RichText::new(format!(
                    "CPU {:.0} % average, {:.0} % peak  ·  memory up to {}  ·  disk {} read, {} written  ·  network {}",
                    s.cpu_avg,
                    s.cpu_max,
                    fmt::bytes(s.memory_max),
                    fmt::bytes(s.disk_read),
                    fmt::bytes(s.disk_write),
                    fmt::bytes(s.net),
                ))
                .size(theme::META)
                .color(theme::TEXT_WEAK),
            );
        }
    });
}

fn summarize(l: &Loaded, from: u32, to: u32) -> Summary {
    #[derive(Default)]
    struct Acc {
        cpu: f64,
        cpu_peak: f32,
        private: u64,
        io: u64,
        disk: u64,
        net: u64,
        gpu: f64,
    }
    let mut s = Summary::default();
    let mut by: HashMap<(u32, Arc<str>), Acc> = HashMap::new();
    let mut cpu_sum = 0.0f64;
    // Consecutive records of the same hang become one span.
    let mut open_hangs: HashMap<(u32, Arc<str>), (u32, u32)> = HashMap::new();
    let start = l.local.partition_point(|&t| t < from);
    let end = l.local.partition_point(|&t| t < to);
    for i in start..end {
        let (b, t) = (&l.buckets[i], l.local[i]);
        let secs = b.secs as f64;
        s.secs += b.secs;
        cpu_sum += b.cpu_avg as f64 * secs;
        s.cpu_max = s.cpu_max.max(b.cpu_max);
        s.memory_max = s.memory_max.max(b.memory_used);
        s.disk_read += b.disk_read;
        s.disk_write += b.disk_write;
        s.net += b.net_rx + b.net_tx;
        for p in &b.processes {
            let a = by.entry((p.pid, p.name.clone())).or_default();
            a.cpu += p.cpu_avg as f64 * secs;
            a.cpu_peak = a.cpu_peak.max(p.cpu_max);
            a.private = a.private.max(p.private);
            a.io += p.io;
            a.disk += p.disk;
            a.net += p.net;
            a.gpu += p.gpu_avg as f64 * secs;
        }
        for e in &b.started {
            s.event_count += e.count;
            s.events.push((t, true, e.clone()));
        }
        for e in &b.exited {
            s.event_count += e.count;
            s.events.push((t, false, e.clone()));
        }
        for e in &b.hung {
            let next = t + BUCKET_SECS as u32;
            match open_hangs.get_mut(&(e.pid, e.name.clone())) {
                Some(span) if span.1 >= t => span.1 = next,
                Some(span) => {
                    s.hangs.push((span.0, span.1, e.pid, e.name.clone()));
                    *span = (t, next);
                }
                None => {
                    open_hangs.insert((e.pid, e.name.clone()), (t, next));
                }
            }
        }
    }
    s.hangs.extend(
        open_hangs
            .into_iter()
            .map(|((pid, name), (a, b))| (a, b, pid, name)),
    );
    s.hangs.sort_by_key(|h| std::cmp::Reverse(h.0));
    if s.secs <= 0.0 {
        return s;
    }
    let secs = s.secs as f64;
    s.cpu_avg = (cpu_sum / secs) as f32;
    s.events.reverse();
    s.events.truncate(ROWS);
    let leads = |value: &dyn Fn(&Acc) -> f64, peak: &dyn Fn(&Acc) -> f64, floor: f64| {
        let mut v: Vec<Lead> = by
            .iter()
            .map(|((pid, name), a)| Lead {
                pid: *pid,
                name: name.clone(),
                value: value(a),
                peak: peak(a),
            })
            .filter(|l| l.value > floor)
            .collect();
        v.sort_by(|a, b| b.value.total_cmp(&a.value));
        v.truncate(ROWS);
        v
    };
    s.cpu = leads(&|a| a.cpu / secs, &|a| a.cpu_peak as f64, 0.01);
    s.memory = leads(&|a| a.private as f64, &|_| 0.0, 0.0);
    s.disk = leads(&|a| a.disk as f64, &|_| 0.0, 0.0);
    if s.disk.is_empty() {
        s.disk_is_io = true;
        s.disk = leads(&|a| a.io as f64, &|_| 0.0, 0.0);
    }
    s.net_leads = leads(&|a| a.net as f64, &|_| 0.0, 0.0);
    s.gpu = leads(&|a| a.gpu / secs, &|_| 0.0, 0.05);
    s
}

fn tables(ui: &mut Ui, s: &Summary, snap: &Snapshot, out: &mut Vec<UiCmd>) {
    // The gap between columns comes on top of the layout's item spacing,
    // which must stay as it is: the cells inside the tables use it too.
    let gap = 12.0;
    let spacing = ui.spacing().item_spacing.x;
    let width = ((ui.available_width() - 2.0 * (gap + 2.0 * spacing)) / 3.0).max(220.0);
    let value = 90.0;
    let name = width - value - 8.0;
    let rate = |l: &Lead| format!("{:.1} %", l.value);
    let bytes = |l: &Lead| fmt::bytes(l.value as u64);
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(width);
            let peak = |l: &Lead| format!("peak {:.0} %", l.peak);
            let peak: &dyn Fn(&Lead) -> String = &peak;
            table(
                ui,
                "CPU",
                "Average",
                &s.cpu,
                name,
                value,
                &rate,
                Some(peak),
                snap,
                out,
            );
        });
        ui.add_space(gap);
        ui.vertical(|ui| {
            ui.set_width(width);
            table(
                ui,
                "Memory",
                "Private, peak",
                &s.memory,
                name,
                value,
                &bytes,
                None,
                snap,
                out,
            );
        });
        ui.add_space(gap);
        ui.vertical(|ui| {
            ui.set_width(width);
            let title = if s.disk_is_io { "I/O" } else { "Disk" };
            table(
                ui,
                title,
                "Read + written",
                &s.disk,
                name,
                value,
                &bytes,
                None,
                snap,
                out,
            );
        });
    });
    ui.add_space(12.0);
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(width);
            table(
                ui,
                "Network",
                "Sent + received",
                &s.net_leads,
                name,
                value,
                &bytes,
                None,
                snap,
                out,
            );
        });
        ui.add_space(gap);
        ui.vertical(|ui| {
            ui.set_width(width);
            table(
                ui, "GPU", "Average", &s.gpu, name, value, &rate, None, snap, out,
            );
        });
        ui.add_space(gap);
        ui.vertical(|ui| {
            ui.set_width(width);
            events(ui, s, width, snap, out);
        });
    });
}

#[allow(clippy::too_many_arguments)]
fn table(
    ui: &mut Ui,
    title: &str,
    value_title: &str,
    leads: &[Lead],
    name_width: f32,
    value_width: f32,
    value: &dyn Fn(&Lead) -> String,
    note: Option<&dyn Fn(&Lead) -> String>,
    snap: &Snapshot,
    out: &mut Vec<UiCmd>,
) {
    widgets::section(ui, title);
    widgets::header(
        ui,
        &[
            ("Process", name_width, Align::Min),
            (value_title, value_width, Align::Max),
        ],
    );
    for (i, l) in leads.iter().enumerate() {
        widgets::row(ui, i, |ui| {
            widgets::cell(ui, name_width, Align::Min, |ui| {
                process_name(ui, l.pid, &l.name, snap, out)
            });
            let cell = widgets::cell(ui, value_width, Align::Max, |ui| ui.label(value(l)));
            if let Some(note) = note {
                cell.on_hover_text(note(l));
            }
        });
    }
    if leads.is_empty() {
        widgets::row(ui, 0, |ui| {
            widgets::text(
                ui,
                name_width,
                Align::Min,
                RichText::new("No process stood out").color(theme::TEXT_DIM),
            );
        });
    }
    widgets::blank_rows(ui, leads.len().max(1), ROWS, name_width);
}

/// A link to the process when it is still running, plain text otherwise.
fn process_name(ui: &mut Ui, pid: u32, name: &str, snap: &Snapshot, out: &mut Vec<UiCmd>) {
    let live = snap
        .processes
        .iter()
        .find(|p| p.pid == pid && p.state != ProcState::Exited && &*p.name == name);
    match live {
        Some(p) => {
            if widgets::link(ui, &format!("{name}  ({pid})"))
                .on_hover_text("Still running: show it in the process list")
                .clicked()
            {
                out.push(UiCmd::GoTo(p.key));
            }
        }
        None => {
            ui.label(RichText::new(format!("{name}  ({pid})")).color(theme::TEXT_WEAK))
                .on_hover_text("Not running any more");
        }
    }
}

fn events(ui: &mut Ui, s: &Summary, width: f32, snap: &Snapshot, out: &mut Vec<UiCmd>) {
    let title = if !s.hangs.is_empty() {
        format!(
            "Events ({} not responding, {} started or exited)",
            s.hangs.len(),
            s.event_count
        )
    } else if s.events.len() >= ROWS {
        format!("Started and exited ({}, latest shown)", s.event_count)
    } else {
        format!("Started and exited ({})", s.event_count)
    };
    widgets::section(ui, &title);
    let time = 64.0;
    let kind = 104.0;
    let name = width - time - kind - 8.0;
    widgets::header(
        ui,
        &[
            ("Time", time, Align::Min),
            ("", kind, Align::Min),
            ("Process", name, Align::Min),
        ],
    );
    // Hangs first: they are what this page is most often opened for.
    let hangs = s.hangs.len().min(ROWS);
    for (i, (from, to, pid, process)) in s.hangs.iter().take(ROWS).enumerate() {
        widgets::row(ui, i, |ui| {
            widgets::text(
                ui,
                time,
                Align::Min,
                RichText::new(hms(*from)).color(theme::TEXT_WEAK),
            );
            widgets::text(
                ui,
                kind,
                Align::Min,
                RichText::new("not responding").color(theme::BAD),
            );
            widgets::cell(ui, name, Align::Min, |ui| {
                process_name(ui, *pid, process, snap, out);
                ui.label(
                    RichText::new(format!("for {} s", to - from))
                        .size(theme::META)
                        .color(theme::TEXT_WEAK),
                );
            });
        });
    }
    let shown = s.events.len().min(ROWS - hangs);
    for (i, (t, started, e)) in s.events.iter().take(shown).enumerate() {
        let i = i + hangs;
        widgets::row(ui, i, |ui| {
            widgets::text(
                ui,
                time,
                Align::Min,
                RichText::new(hms(*t)).color(theme::TEXT_WEAK),
            );
            let (label, color) = if *started {
                ("started", theme::GOOD)
            } else {
                ("exited", theme::TEXT_WEAK)
            };
            let label = if e.count > 1 {
                format!("{label} ×{}", e.count)
            } else {
                label.to_string()
            };
            widgets::text(ui, kind, Align::Min, RichText::new(label).color(color));
            widgets::cell(ui, name, Align::Min, |ui| {
                if e.count > 1 {
                    ui.label(&*e.name);
                } else {
                    process_name(ui, e.pid, &e.name, snap, out);
                }
            });
        });
    }
    widgets::blank_rows(ui, hangs + shown, ROWS, name);
}
