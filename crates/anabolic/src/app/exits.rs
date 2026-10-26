// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! "Notify me when it exits": the processes to tell about, and the telling.
//! Once one is gone the collector is asked for its record, which has its
//! exit code when the process trace saw it end (as administrator); the
//! notification waits a moment for that answer, then goes without it.

use anabolic_core::collector::Ancestry;

use super::*;

/// How long the collector's record of an exited process is waited for.
const RECORD_WAIT: Duration = Duration::from_secs(2);
/// The icon stays this long after its notification: Windows can take a
/// notification away with the icon it came from.
const LINGER: Duration = Duration::from_secs(30);

struct Watched {
    key: ProcKey,
    name: Arc<str>,
    /// Asked for its record once it was gone: when, and where the answer comes.
    asked: Option<(Instant, Receiver<Ancestry>)>,
}

#[derive(Default)]
pub(super) struct ExitWatch {
    list: Vec<Watched>,
    /// When the last notification went out from the icon.
    told_at: Option<Instant>,
}

impl ExitWatch {
    pub(super) fn contains(&self, key: ProcKey) -> bool {
        self.list.iter().any(|w| w.key == key)
    }

    /// Whether the notification area icon is needed for these: one to tell
    /// about, or one told about a moment ago.
    pub(super) fn needs_icon(&self) -> bool {
        !self.list.is_empty() || self.told_at.is_some_and(|at| at.elapsed() < LINGER)
    }

    /// The processes watched, for the process menu.
    pub(super) fn keys(&self) -> Vec<ProcKey> {
        self.list.iter().map(|w| w.key).collect()
    }
}

/// How it ended, from its exit code.
#[cfg(windows)]
fn how(code: u32) -> String {
    match code {
        0xC000013A => "after Ctrl+C".into(),
        0xC0000005 => "by crashing (access violation, 0xC0000005)".into(),
        0xC0000409 => "by crashing (stack buffer overrun, 0xC0000409)".into(),
        0xC00000FD => "by crashing (stack overflow, 0xC00000FD)".into(),
        0xC0000374 => "by crashing (heap corruption, 0xC0000374)".into(),
        c if c >= 0xC000_0000 => format!("with error 0x{c:08X}"),
        c => format!("with exit code {c}"),
    }
}

/// How it ended, from its wait status: the signal that ended it in the low
/// bits, else the exit code above them.
#[cfg(target_os = "linux")]
fn how(status: u32) -> String {
    match status & 0x7F {
        0 => format!("with exit code {}", (status >> 8) & 0xFF),
        11 => "by crashing (SIGSEGV)".into(),
        6 => "by aborting (SIGABRT)".into(),
        9 => "killed (SIGKILL)".into(),
        15 => "when told to (SIGTERM)".into(),
        2 => "after Ctrl+C (SIGINT)".into(),
        signal => format!("on signal {signal}"),
    }
}

/// The notification's text: how long it ran, and how it ended when known.
fn ended(key: ProcKey, exited: Option<(i64, Option<u32>)>) -> String {
    let end = exited.map_or_else(anabolic_core::sysinfo::now_filetime, |(at, _)| at);
    let ran = Duration::from_secs(((end - key.create_time).max(0) / 10_000_000) as u64);
    match exited.and_then(|(_, code)| code) {
        Some(code) => format!(
            "PID {} ran for {} and ended {}.",
            key.pid,
            fmt::span(ran),
            how(code)
        ),
        None => format!("PID {} ran for {}.", key.pid, fmt::span(ran)),
    }
}

impl App {
    pub(super) fn notify_exit(&mut self, key: ProcKey, on: bool) {
        if !on {
            self.exit_watch.list.retain(|w| w.key != key);
            return;
        }
        let Some(p) = self.snapshot.find(key) else {
            return;
        };
        if p.state == ProcState::Exited || self.exit_watch.contains(key) {
            return;
        }
        let name = p.name.clone();
        self.toasts.info(format!(
            "Anabolic will tell you when {name} (PID {}) exits",
            key.pid
        ));
        self.exit_watch.list.push(Watched {
            key,
            name,
            asked: None,
        });
    }

    /// Every refresh: the watched processes that are gone are told about.
    pub(super) fn exit_logic(&mut self, ctx: &egui::Context) {
        let mut told = Vec::new();
        for w in &mut self.exit_watch.list {
            let gone = self
                .snapshot
                .find(w.key)
                .is_none_or(|p| p.state == ProcState::Exited);
            if !gone {
                continue;
            }
            // When it was first asked about, and its record if one came.
            let (since, record) = match &w.asked {
                None => (Instant::now(), None),
                Some((at, rx)) => match rx.try_recv() {
                    Ok(record) => (*at, Some(record.origin.and_then(|o| o.exited))),
                    Err(_) if at.elapsed() < RECORD_WAIT => {
                        ctx.request_repaint_after(Duration::from_millis(100));
                        continue;
                    }
                    Err(_) => {
                        told.push((w.key, w.name.clone(), None));
                        continue;
                    }
                },
            };
            // The trace's stop event, which has the exit code, can come in
            // after the scan saw the process gone: ask again for a moment.
            if let Some(exited) = record
                && (exited.is_some_and(|(_, code)| code.is_some())
                    || since.elapsed() >= RECORD_WAIT)
            {
                told.push((w.key, w.name.clone(), exited));
                continue;
            }
            let (tx, rx) = crossbeam_channel::bounded(1);
            self.collector.send(Command::Ancestry(w.key, tx));
            w.asked = Some((since, rx));
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        for (key, name, exited) in told {
            self.exit_watch.list.retain(|w| w.key != key);
            let title = format!("{name} has exited");
            let text = ended(key, exited);
            // Seen in the window when it is in front; otherwise from the
            // notification area, which needs its icon.
            let in_front = !self.hidden && ctx.input(|i| i.viewport().focused.unwrap_or(false));
            match &self.tray {
                Some(tray) if !in_front => {
                    tray.notify(&title, &text, false);
                    self.exit_watch.told_at = Some(Instant::now());
                }
                _ => self.toasts.info(format!("{title}. {text}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_how_long_it_ran_and_how_it_ended() {
        let key = ProcKey {
            pid: 42,
            create_time: 1_000 * 10_000_000,
        };
        let at = key.create_time + 125 * 10_000_000;
        assert_eq!(ended(key, Some((at, None))), "PID 42 ran for 2 min.");
        #[cfg(windows)]
        {
            assert_eq!(
                ended(key, Some((at, Some(0)))),
                "PID 42 ran for 2 min and ended with exit code 0."
            );
            assert!(how(0xC0000005).contains("access violation"));
            assert_eq!(how(0xC0000135), "with error 0xC0000135");
        }
        #[cfg(target_os = "linux")]
        {
            assert_eq!(how(3 << 8), "with exit code 3");
            assert_eq!(how(9), "killed (SIGKILL)");
        }
    }
}
