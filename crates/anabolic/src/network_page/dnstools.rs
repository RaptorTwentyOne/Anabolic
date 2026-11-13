// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Network > Hosts and DNS: which DNS server answers fastest from here
//! (the one in use and the well-known public ones, timed in the manner of
//! GRC's DNS Benchmark), what the DNS cache holds, emptied on request,
//! and the hosts file, edited and saved after a question.

use anabolic_core::dnscache::Cached;
use anabolic_core::dnstools::{self, Timing};
use crossbeam_channel::{Receiver, bounded};
use eframe::egui::{self, Label, RichText, Ui};

use crate::proc_table::UiCmd;
use crate::{theme, widgets};

#[derive(Default)]
pub struct DnsToolsView {
    timings: Option<Vec<Timing>>,
    timing: Option<Receiver<Vec<Timing>>>,
    cache: Option<Vec<Cached>>,
    cache_reading: Option<Receiver<Vec<Cached>>>,
    cache_filter: String,
    /// The hosts file as read, and as being edited.
    hosts_read: Option<Result<String, String>>,
    hosts: String,
}

fn spawn<T: Send + 'static>(
    ctx: &egui::Context,
    name: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<Receiver<T>> {
    let (tx, rx) = bounded(1);
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        })
        .ok()
        .map(|_| rx)
}

impl DnsToolsView {
    fn poll(&mut self) {
        if let Some(rx) = &self.timing
            && let Ok(t) = rx.try_recv()
        {
            self.timings = Some(t);
            self.timing = None;
        }
        if let Some(rx) = &self.cache_reading
            && let Ok(c) = rx.try_recv()
        {
            self.cache = Some(c);
            self.cache_reading = None;
        }
    }

    fn read_hosts(&mut self) {
        let read = dnstools::read_hosts();
        self.hosts = read.clone().unwrap_or_default();
        self.hosts_read = Some(read);
    }
}

fn ms(v: Option<f32>) -> RichText {
    match v {
        Some(v) => RichText::new(format!("{v:.0} ms")),
        None => RichText::new("no answer").color(theme::BAD),
    }
}

pub fn show(ui: &mut Ui, view: &mut DnsToolsView, out: &mut Vec<UiCmd>) {
    view.poll();
    if view.hosts_read.is_none() {
        view.read_hosts();
    }
    if view.cache.is_none() && view.cache_reading.is_none() {
        view.cache_reading = spawn(ui.ctx(), "dns-cache", anabolic_core::dnscache::entries);
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            speed(ui, view);
            ui.add_space(14.0);
            cache(ui, view, out);
            ui.add_space(14.0);
            hosts(ui, view, out);
            ui.add_space(10.0);
        });
}

fn speed(ui: &mut Ui, view: &mut DnsToolsView) {
    ui.horizontal(|ui| {
        widgets::section(ui, "DNS server speed");
        let running = view.timing.is_some();
        if ui
            .add_enabled(!running, egui::Button::new("Test"))
            .on_hover_text(
                "Asks each server for a dozen popular names twice, and for names it cannot have \
                 cached: a few hundred small queries in all.",
            )
            .clicked()
        {
            view.timing = spawn(ui.ctx(), "dns-benchmark", || {
                dnstools::benchmark(dnstools::resolvers())
            });
        }
        if running {
            ui.spinner();
        }
    });
    ui.add(
        Label::new(
            RichText::new(
                "Every name a program opens is looked up first. The DNS servers in use and the \
                 well-known public ones, timed from here in the manner of GRC's DNS Benchmark: \
                 an answer from the server's cache, and one it had to look up.",
            )
            .size(theme::META)
            .color(theme::TEXT_WEAK),
        )
        .wrap(),
    );
    let Some(timings) = &view.timings else {
        return;
    };
    ui.add_space(4.0);
    ui.label(widgets::strong(dnstools::verdict(timings), theme::BODY).color(theme::TEXT));
    ui.add_space(4.0);
    egui::Grid::new("dns-timings")
        .num_columns(5)
        .spacing([18.0, 3.0])
        .striped(true)
        .show(ui, |ui| {
            for title in ["Server", "Address", "From its cache", "Looked up", "Lost"] {
                ui.label(RichText::new(title).strong().color(theme::TEXT_WEAK));
            }
            ui.end_row();
            for t in timings {
                let name = if t.resolver.yours {
                    format!("{} (in use)", t.resolver.name)
                } else {
                    t.resolver.name.clone()
                };
                ui.label(RichText::new(name).color(if t.resolver.yours {
                    theme::ACCENT
                } else {
                    theme::TEXT
                }));
                ui.label(RichText::new(t.resolver.addr.to_string()).color(theme::TEXT_WEAK));
                ui.label(ms(t.cached_ms));
                ui.label(ms(t.uncached_ms).color(theme::TEXT_WEAK));
                ui.label(
                    RichText::new(format!("{}/{}", t.lost, t.sent)).color(if t.lost > 0 {
                        theme::ELEVATED_TEXT
                    } else {
                        theme::TEXT_WEAK
                    }),
                );
                ui.end_row();
            }
        });
    ui.add(
        Label::new(
            RichText::new(if cfg!(windows) {
                "The DNS server is set per adapter: Settings > Network & internet > the adapter \
                 > DNS server assignment."
            } else {
                "The DNS server is set in the network's settings (NetworkManager, or \
                 systemd-resolved's DNS=)."
            })
            .size(theme::META)
            .color(theme::TEXT_DIM),
        )
        .wrap(),
    );
}

fn cache(ui: &mut Ui, view: &mut DnsToolsView, out: &mut Vec<UiCmd>) {
    ui.horizontal(|ui| {
        widgets::section(ui, "DNS cache");
        if ui.button("Read again").clicked() && view.cache_reading.is_none() {
            view.cache_reading = spawn(ui.ctx(), "dns-cache", anabolic_core::dnscache::entries);
        }
        if ui
            .button("Empty it")
            .on_hover_text("As ipconfig /flushdns: names are looked up afresh")
            .clicked()
        {
            out.push(UiCmd::FlushDns);
            view.cache = Some(Vec::new());
        }
        ui.add_space(8.0);
        widgets::clearable(ui, &mut view.cache_filter, |t| {
            egui::TextEdit::singleline(t)
                .hint_text("Filter names")
                .desired_width(200.0)
        });
    });
    let Some(cache) = &view.cache else {
        ui.spinner();
        return;
    };
    if cache.is_empty() {
        ui.label(
            RichText::new(if cfg!(windows) {
                "The cache is empty."
            } else {
                "systemd-resolved shows its cache to root only; it can still be emptied."
            })
            .color(theme::TEXT_WEAK),
        );
        return;
    }
    let f = view.cache_filter.trim().to_lowercase();
    let mut shown: Vec<&Cached> = cache
        .iter()
        .filter(|c| f.is_empty() || c.name.to_lowercase().contains(&f))
        .collect();
    // The ones with an answer first.
    shown.sort_by_key(|c| c.addresses.is_empty());
    ui.label(
        RichText::new(format!("{} names, {} shown", cache.len(), shown.len()))
            .size(theme::META)
            .color(theme::TEXT_WEAK),
    );
    egui::ScrollArea::vertical()
        .id_salt("dns-cache")
        .max_height(240.0)
        .show(ui, |ui| {
            egui::Grid::new("dns-cache-grid")
                .num_columns(2)
                .spacing([18.0, 2.0])
                .striped(true)
                .show(ui, |ui| {
                    for c in shown.iter().take(500) {
                        ui.label(&c.name);
                        let addresses: Vec<String> =
                            c.addresses.iter().map(|a| a.to_string()).collect();
                        let text = if addresses.is_empty() {
                            "no address held now (expired, not found, or another kind)".to_string()
                        } else {
                            addresses.join(", ")
                        };
                        ui.label(RichText::new(text).color(theme::TEXT_WEAK));
                        ui.end_row();
                    }
                });
        });
}

fn hosts(ui: &mut Ui, view: &mut DnsToolsView, out: &mut Vec<UiCmd>) {
    let changed = view
        .hosts_read
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .is_some_and(|read| *read != view.hosts);
    ui.horizontal(|ui| {
        widgets::section(ui, "Hosts file");
        if ui
            .add_enabled(changed, egui::Button::new("Save…"))
            .clicked()
        {
            out.push(UiCmd::ConfirmHosts(view.hosts.clone()));
        }
        if ui
            .add_enabled(changed, egui::Button::new("Undo changes"))
            .clicked()
        {
            view.read_hosts();
        }
        if ui.button("Read again").clicked() {
            view.read_hosts();
        }
    });
    let entries = dnstools::parse_hosts(&view.hosts);
    let on = entries.iter().filter(|e| e.enabled).count();
    ui.add(
        Label::new(
            RichText::new(format!(
                "{}: names here go to the address beside them before any DNS server is asked. \
                 {on} line{} in use, {} turned off with #.",
                dnstools::hosts_path().display(),
                if on == 1 { "" } else { "s" },
                entries.len() - on
            ))
            .size(theme::META)
            .color(theme::TEXT_WEAK),
        )
        .wrap(),
    );
    if let Some(Err(e)) = &view.hosts_read {
        ui.label(RichText::new(format!("It could not be read: {e}")).color(theme::BAD));
        return;
    }
    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .id_salt("hosts-text")
        .max_height(320.0)
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut view.hosts)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(12),
            );
        });
}
