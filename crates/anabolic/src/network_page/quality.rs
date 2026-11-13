// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Network > Quality: a ping a second to the router, the DNS server, the
//! internet and a game's server (picked from the program's busiest
//! connection), each with its delay, jitter and lost replies and a strip
//! of the last two minutes, the trouble in words, and a route traced hop
//! by hop, in the manner of PingPlotter and WinMTR. The pings run while
//! the tab is open, and on with it closed when asked.

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use anabolic_core::model::NetActivity;
use anabolic_core::netquality::{self, Hop, Kind, Monitor, Series, Stats, Target};
use anabolic_core::nettrace::Proto;
use anabolic_core::{ProcState, Snapshot};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Align, Color32, Label, Layout, RichText, Sense, Ui, vec2};

use crate::netnames::Names;
use crate::{theme, widgets};

/// The pings stop this long after the tab is left, unless kept on.
const LINGER: Duration = Duration::from_secs(60);
/// The verdict and the table judge the last minute.
const SPAN: Duration = Duration::from_secs(60);
/// The strip shows this many pings.
const STRIP: usize = 120;

/// A traced route, or why it could not be.
type Traced = Result<Vec<Hop>, String>;

#[derive(Default)]
pub struct QualityView {
    monitors: Vec<Monitor>,
    /// Pings on with the tab closed.
    pub keep: bool,
    /// The game server picked by hand; None picks the busiest.
    server: Option<Ipv4Addr>,
    shown_at: Option<Instant>,
    add: String,
    route: Option<(Ipv4Addr, Traced)>,
    tracing: Option<(Ipv4Addr, Receiver<Traced>)>,
}

/// A public endpoint a program exchanges traffic with now.
#[derive(Clone, Debug, PartialEq)]
struct Candidate {
    addr: Ipv4Addr,
    port: u16,
    proto: Proto,
    pid: u32,
    program: String,
    rate: f64,
}

/// The busiest public endpoints, busiest first, one per address; a game
/// in front puts its own first.
fn candidates(snap: &Snapshot, activity: &NetActivity, game: Option<u32>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for c in activity.connections.iter().filter(|c| c.idle < 5) {
        let IpAddr::V4(addr) = c.remote else {
            continue;
        };
        if !netquality::public(addr) {
            continue;
        }
        let rate = c.rx_rate + c.tx_rate;
        let program = snap
            .processes
            .iter()
            .find(|p| p.pid == c.pid && p.state != ProcState::Exited)
            .map_or_else(|| format!("PID {}", c.pid), |p| p.name.to_string());
        match out.iter_mut().find(|x| x.addr == addr) {
            Some(x) => x.rate += rate,
            None => out.push(Candidate {
                addr,
                port: c.remote_port,
                proto: c.proto,
                pid: c.pid,
                program,
                rate,
            }),
        }
    }
    out.sort_by(|a, b| {
        (Some(b.pid) == game)
            .cmp(&(Some(a.pid) == game))
            .then(b.rate.total_cmp(&a.rate))
    });
    out.truncate(12);
    out
}

impl QualityView {
    /// Stops the pings once the tab has been closed a while; call every
    /// frame.
    pub fn tick(&mut self, shown: bool) {
        if shown {
            self.shown_at = Some(Instant::now());
        } else if !self.keep
            && !self.monitors.is_empty()
            && self.shown_at.is_none_or(|t| t.elapsed() > LINGER)
        {
            self.monitors.clear();
        }
    }

    fn watched(&self) -> Vec<Target> {
        self.monitors.iter().flat_map(|m| m.targets()).collect()
    }

    fn watch(&mut self, t: Target) {
        if !self.watched().iter().any(|w| w.addr == t.addr) {
            self.monitors.push(Monitor::start(vec![t]));
        }
    }

    fn unwatch(&mut self, addr: Ipv4Addr) {
        self.monitors
            .retain(|m| m.targets().iter().all(|t| t.addr != addr));
    }

    fn trace(&mut self, to: Ipv4Addr, ctx: &egui::Context) {
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("trace-route".into())
            .spawn(move || {
                let _ = tx.send(netquality::trace(to, 3));
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.tracing = Some((to, rx));
        }
    }
}

fn verdict_color(text: &str) -> Color32 {
    if text.starts_with("No trouble") {
        theme::GOOD
    } else if text.starts_with("Measuring") {
        theme::TEXT_WEAK
    } else {
        theme::ELEVATED_TEXT
    }
}

fn ms(v: f32) -> String {
    if v < 10.0 {
        format!("{v:.1} ms")
    } else {
        format!("{v:.0} ms")
    }
}

/// The last pings as bars: taller is slower, red is lost.
fn strip(ui: &mut Ui, s: &Series) {
    let (rect, _) = ui.allocate_exact_size(vec2(240.0, 18.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::RAISED);
    let samples: Vec<Option<f32>> = s
        .samples
        .iter()
        .rev()
        .take(STRIP)
        .map(|(_, v)| *v)
        .collect();
    let top = samples.iter().flatten().copied().fold(100.0f32, f32::max);
    let w = rect.width() / STRIP as f32;
    for (i, v) in samples.iter().enumerate() {
        let x = rect.right() - (i + 1) as f32 * w;
        let (h, color) = match v {
            Some(ms) => ((ms / top).clamp(0.08, 1.0) * rect.height(), theme::NET_RX),
            None => (rect.height(), theme::BAD),
        };
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x, rect.bottom() - h),
                egui::pos2(x + w.max(1.0) - 0.5, rect.bottom()),
            ),
            0.0,
            color,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    view: &mut QualityView,
    snap: &Snapshot,
    activity: &NetActivity,
    game: Option<u32>,
    names: &mut Names,
) {
    view.tick(true);
    if view.monitors.is_empty() {
        for t in netquality::targets() {
            view.watch(t);
        }
    }
    if let Some((to, rx)) = &view.tracing
        && let Ok(hops) = rx.try_recv()
    {
        view.route = Some((*to, hops));
        view.tracing = None;
    }
    // The game server: picked by hand, or the busiest endpoint.
    let found = candidates(snap, activity, game);
    let server = view
        .server
        .or_else(|| found.first().map(|c| c.addr))
        .and_then(|a| found.iter().find(|c| c.addr == a).cloned());
    let watched_server = view.watched().into_iter().find(|t| t.kind == Kind::Server);
    if let Some(c) = &server
        && watched_server.as_ref().is_none_or(|t| t.addr != c.addr)
    {
        if let Some(old) = watched_server {
            view.unwatch(old.addr);
        }
        view.watch(Target {
            kind: Kind::Server,
            addr: c.addr,
            label: format!("{} ({})", c.program, c.addr),
        });
    }

    let series: Vec<Series> = view.monitors.iter().flat_map(|m| m.series()).collect();
    let of = |k: Kind| {
        series
            .iter()
            .find(|s| s.target.kind == k)
            .map(|s| s.stats(SPAN))
    };
    let verdict = netquality::verdict(of(Kind::Router), of(Kind::Internet), of(Kind::Server));
    ui.horizontal(|ui| {
        ui.add(
            Label::new(
                RichText::new(&verdict)
                    .size(theme::SUBTITLE)
                    .color(verdict_color(&verdict)),
            )
            .wrap(),
        );
    });
    ui.add(
        Label::new(
            RichText::new(
                "A ping a second to each, judged over the last minute, as PingPlotter and WinMTR \
                 do. Only ICMP echo requests are sent. A server that does not answer pings may \
                 still be fine: its route shows how far replies come.",
            )
            .size(theme::META)
            .color(theme::TEXT_WEAK),
        )
        .wrap(),
    );
    ui.add_space(6.0);

    let mut remove = None;
    let mut trace = None;
    egui::Grid::new("quality")
        .num_columns(9)
        .spacing([14.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            for title in [
                "",
                "Address",
                "Now",
                "Average",
                "Best",
                "Worst",
                "Jitter",
                "Lost",
                "Last 2 min",
            ] {
                ui.label(RichText::new(title).strong().color(theme::TEXT_WEAK));
            }
            ui.end_row();
            for s in &series {
                let st: Stats = s.stats(SPAN);
                ui.label(widgets::strong(s.target.kind.label(), theme::BODY));
                ui.label(RichText::new(&s.target.label).color(theme::TEXT_WEAK));
                let silent = st.silent();
                ui.label(match st.last {
                    Some(v) => RichText::new(ms(v)),
                    None if silent => RichText::new("does not answer pings").color(theme::TEXT_DIM),
                    None => RichText::new("lost").color(theme::BAD),
                });
                for v in [st.avg, st.min, st.max, st.jitter] {
                    ui.label(if st.sent > st.lost {
                        RichText::new(ms(v)).color(theme::TEXT_WEAK)
                    } else {
                        RichText::new("—").color(theme::TEXT_DIM)
                    });
                }
                let loss = st.loss_pct();
                ui.label(RichText::new(format!("{loss:.0} %")).color(if silent {
                    theme::TEXT_DIM
                } else if loss >= 2.0 {
                    theme::BAD
                } else {
                    theme::TEXT_WEAK
                }));
                ui.horizontal(|ui| {
                    strip(ui, s);
                    if ui
                        .small_button("Route")
                        .on_hover_text("Trace the route to it, hop by hop")
                        .clicked()
                    {
                        trace = Some(s.target.addr);
                    }
                    if s.target.kind == Kind::Other
                        && ui
                            .small_button(widgets::icon(theme::icon::CLOSE, 9.0))
                            .on_hover_text("Stop pinging it")
                            .clicked()
                    {
                        remove = Some(s.target.addr);
                    }
                });
                ui.end_row();
            }
        });
    if let Some(a) = remove {
        view.unwatch(a);
    }
    if let Some(a) = trace
        && view.tracing.is_none()
    {
        view.trace(a, ui.ctx());
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label("Server").on_hover_text(
            "The busiest address on the internet a program talks to now; a game in front \
             comes first.",
        );
        let text = server.as_ref().map_or_else(
            || "none: no program talks to the internet now".to_string(),
            |c| {
                format!(
                    "{} → {}:{} ({}, {}/s)",
                    c.program,
                    c.addr,
                    c.port,
                    match c.proto {
                        Proto::Tcp => "TCP",
                        Proto::Udp => "UDP",
                    },
                    crate::fmt::bytes(c.rate as u64)
                )
            },
        );
        egui::ComboBox::from_id_salt("quality-server")
            .selected_text(text)
            .width(380.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(view.server.is_none(), "The busiest, the game's first")
                    .clicked()
                {
                    view.server = None;
                }
                for c in &found {
                    let label = format!(
                        "{} → {}:{} ({}/s)",
                        c.program,
                        c.addr,
                        c.port,
                        crate::fmt::bytes(c.rate as u64)
                    );
                    if ui
                        .selectable_label(view.server == Some(c.addr), label)
                        .clicked()
                    {
                        view.server = Some(c.addr);
                    }
                }
            });
        ui.add_space(12.0);
        let add = ui.add(
            egui::TextEdit::singleline(&mut view.add)
                .hint_text("Ping another address")
                .desired_width(160.0),
        );
        let entered = add.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.button("Add").clicked() || entered)
            && let Ok(a) = view.add.trim().parse::<Ipv4Addr>()
        {
            view.watch(Target {
                kind: Kind::Other,
                addr: a,
                label: a.to_string(),
            });
            view.add.clear();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.checkbox(&mut view.keep, "Keep pinging with this tab closed");
        });
    });

    ui.add_space(10.0);
    if let Some((to, _)) = &view.tracing {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(
                RichText::new(format!("Tracing the route to {to}, three pings a hop…"))
                    .color(theme::TEXT_WEAK),
            );
        });
    }
    if let Some((to, hops)) = &view.route {
        route(ui, *to, hops, names);
    }
}

fn route(ui: &mut Ui, to: Ipv4Addr, hops: &Traced, names: &mut Names) {
    widgets::section(ui, &format!("Route to {to}"));
    let hops = match hops {
        Ok(h) => h,
        Err(e) => {
            ui.label(RichText::new(e).color(theme::BAD));
            return;
        }
    };
    ui.add(
        Label::new(
            RichText::new(
                "Delay or loss that starts at one hop and carries on to the end shows where the \
                 trouble begins. A hop that is slow or loses replies while the ones after it do \
                 not is only a router that answers pings last.",
            )
            .size(theme::META)
            .color(theme::TEXT_WEAK),
        )
        .wrap(),
    );
    let ctx = ui.ctx().clone();
    egui::Grid::new(("route", to))
        .num_columns(5)
        .spacing([16.0, 3.0])
        .striped(true)
        .show(ui, |ui| {
            for title in ["Hop", "Address", "Name", "Average", "Lost"] {
                ui.label(RichText::new(title).strong().color(theme::TEXT_WEAK));
            }
            ui.end_row();
            for h in hops {
                let st = h.stats();
                ui.label(h.ttl.to_string());
                match h.addr {
                    Some(a) => {
                        ui.label(a.to_string());
                        let name = match names.get(&ctx, IpAddr::V4(a)) {
                            crate::netnames::Name::Dns(n) => n.to_string(),
                            crate::netnames::Name::Kind(k) => k.to_string(),
                            _ => String::new(),
                        };
                        ui.label(RichText::new(name).color(theme::TEXT_WEAK));
                    }
                    None => {
                        ui.label(RichText::new("no answer").color(theme::TEXT_DIM));
                        ui.label("");
                    }
                }
                ui.label(if h.ms.is_empty() {
                    RichText::new("—").color(theme::TEXT_DIM)
                } else {
                    RichText::new(ms(st.avg))
                });
                ui.label(RichText::new(format!("{:.0} %", st.loss_pct())).color(
                    if h.addr.is_some() && st.loss_pct() > 0.0 {
                        theme::ELEVATED_TEXT
                    } else {
                        theme::TEXT_WEAK
                    },
                ));
                ui.end_row();
            }
        });
}
