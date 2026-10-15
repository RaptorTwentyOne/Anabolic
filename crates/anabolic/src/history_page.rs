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

pub fn show(
    ui: &mut Ui,
    page: &mut HistoryPage,
    snap: &Snapshot,
    dir: Option<PathBuf>,
    recording: bool,
    keep_days: u32,
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

    header(ui, page, &today, recording, keep_days);
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

fn header(ui: &mut Ui, page: &mut HistoryPage, today: &str, recording: bool, keep_days: u32) {
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
        response.on_hover_text(format!("{}: show this hour", hm(t_at(rect, 0, DAY, pos.x))));
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

/// One hour at full resolution: CPU average as bars with its peak as a
/// tick, memory as a line. Drag to pick a range, click to pick a record,
/// double-click to clear.
fn hour_graph(
    ui: &mut Ui,
    l: &Loaded,
    window: u32,
    selection: &mut Option<(u32, u32)>,
    drag_from: &mut Option<u32>,
) {
    ui.horizontal(|ui| {
        let swatch = |ui: &mut Ui, color: Color32, text: &str| {
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, color);
            ui.label(
                RichText::new(text)
                    .size(theme::META)
                    .color(theme::TEXT_WEAK),
            );
            ui.add_space(8.0);
        };
        swatch(ui, theme::CPU, "CPU, average (the tick is the peak)");
        swatch(ui, theme::MEMORY, "Memory in use");
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new("Drag to pick a range, double-click to clear")
                    .size(theme::META)
                    .color(theme::TEXT_DIM),
            );
        });
    });
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 170.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect.expand(1.0));
    painter.rect_filled(rect, 2.0, theme::BG);
    let grid = Stroke::new(1.0, theme::GRID);
    for pct in [25.0, 50.0, 75.0] {
        let y = painter.round_to_pixel_center(rect.bottom() - rect.height() * pct / 100.0);
        painter.hline(rect.x_range(), y, grid);
    }
    for m in (10..60).step_by(10) {
        let x = painter.round_to_pixel_center(x_at(rect, window, HOUR, window + m * 60));
        painter.vline(x, rect.y_range(), grid);
    }

    let first = l.local.partition_point(|&t| t < window);
    let end = l.local.partition_point(|&t| t < window + HOUR);
    let y_of = |pct: f32| rect.bottom() - (pct / 100.0).clamp(0.0, 1.0) * rect.height();
    let step = rect.width() * BUCKET_SECS as f32 / HOUR as f32;
    let bar = theme::CPU.gamma_multiply(0.55);
    let mut memory: Vec<Pos2> = Vec::new();
    let mut lines: Vec<Vec<Pos2>> = Vec::new();
    let mut previous: Option<u32> = None;
    for i in first..end {
        let (b, t) = (&l.buckets[i], l.local[i]);
        let x0 = x_at(rect, window, HOUR, t);
        let x1 = (x0 + step - 0.5).max(x0 + 1.0);
        painter.rect_filled(
            Rect::from_x_y_ranges(x0..=x1, y_of(b.cpu_avg)..=rect.bottom()),
            0.0,
            bar,
        );
        let peak = y_of(b.cpu_max);
        painter.hline(x0..=x1, peak, Stroke::new(1.0, theme::CPU));
        // The memory line breaks where nothing was recorded.
        if previous.is_some_and(|p| t > p + BUCKET_SECS as u32) {
            lines.push(std::mem::take(&mut memory));
        }
        memory.push(Pos2::new(x0 + step / 2.0, y_of(memory_pct(b))));
        previous = Some(t);
    }
    lines.push(memory);
    for line in lines {
        if line.len() > 1 {
            painter.add(Shape::line(line, Stroke::new(1.5, theme::MEMORY)));
        }
    }

    // Picking: snapped to records.
    let snap_t = |x: f32| t_at(rect, window, HOUR, x) / BUCKET_SECS as u32 * BUCKET_SECS as u32;
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
            x_at(rect, window, HOUR, a)..=x_at(rect, window, HOUR, b),
            rect.y_range(),
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
            rect.y_range(),
            Stroke::new(1.0, theme::BORDER_STRONG),
        );
        let hovered = l.local[first..end]
            .binary_search(&t)
            .ok()
            .map(|i| i + first);
        response.on_hover_ui_at_pointer(|ui| match hovered {
            Some(i) => bucket_tooltip(ui, &l.buckets[i], l.local[i]),
            None => {
                ui.label(format!("{}: not recorded", hms(t)));
            }
        });
    }

    let (labels, _) = ui.allocate_exact_size(vec2(rect.width(), 14.0), Sense::hover());
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

fn bucket_tooltip(ui: &mut Ui, b: &Bucket, t: u32) {
    ui.label(widgets::strong(
        format!("{} – {}", hms(t), hms(t + BUCKET_SECS as u32)),
        theme::BODY,
    ));
    ui.label(format!(
        "CPU {:.0} % average, {:.0} % peak",
        b.cpu_avg, b.cpu_max
    ));
    ui.label(format!(
        "Memory {} of {}",
        fmt::bytes(b.memory_used),
        fmt::bytes(b.memory_total)
    ));
    if b.disk_active > 0.0 {
        ui.label(format!("Busiest disk {:.0} % active", b.disk_active));
    }
    if let Some(p) = b.processes.first().filter(|p| p.cpu_avg > 0.1) {
        ui.label(
            RichText::new(format!("Top CPU: {} ({:.1} %)", p.name, p.cpu_avg))
                .color(theme::TEXT_WEAK),
        );
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
    }
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
    let title = if s.events.len() >= ROWS {
        format!("Started and exited ({}, latest shown)", s.event_count)
    } else {
        format!("Started and exited ({})", s.event_count)
    };
    widgets::section(ui, &title);
    let time = 64.0;
    let kind = 76.0;
    let name = width - time - kind - 8.0;
    widgets::header(
        ui,
        &[
            ("Time", time, Align::Min),
            ("", kind, Align::Min),
            ("Process", name, Align::Min),
        ],
    );
    for (i, (t, started, e)) in s.events.iter().enumerate() {
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
    widgets::blank_rows(ui, s.events.len(), ROWS, name);
}
