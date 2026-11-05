// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Boots from Windows' event logs. The System log has, for every boot, the
//! kernel's start (Kernel-General 12), the firmware's timings (Kernel-Boot
//! 30), the kind of start (Kernel-Boot 27), whether the last shutdown was
//! clean (Kernel-Boot 20, Kernel-Power 41, EventLog 6008 with the last
//! moment it ran, a stop error's dump from WER 1001), who asked for the
//! shutdown before and why (User32 1074) and when the kernel went down
//! (Kernel-General 13), the services and drivers that failed (Service
//! Control Manager) and the sleeps (Kernel-Power 42). Winlogon's own log
//! has the sign-in screen, the sign-in and the desktop, with each step's
//! time; Group Policy's, its time at the start and at the sign-in.
//! Diagnostics-Performance adds Windows' own measurement and what slowed a
//! boot, on the machines where it still keeps them.

use std::time::Instant;

use windows::Win32::System::EventLog::{
    EVT_HANDLE, EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EvtQueryTolerateQueryErrors,
    EvtRender, EvtRenderEventXml,
};
use windows::core::PCWSTR;

use crate::boot::{
    Boot, BootKind, Boots, Measured, Problem, Shutdown, Slow, SlowKind, filetime_of_iso,
};
use crate::util::wide;

/// How far back: the logs are small and roll over long before this.
const DAYS: u64 = 120;
const MAX_BOOTS: usize = 60;
const SECOND: i64 = 10_000_000;

/// One event, as much of it as is used.
#[derive(Debug, Default)]
struct Event {
    provider: String,
    id: u32,
    /// When it was logged, as a FILETIME.
    at: i64,
    data: Vec<(String, String)>,
    /// The event's binary data.
    binary: Vec<u8>,
}

impl Event {
    fn get(&self, name: &str) -> Option<&str> {
        self.data
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn num(&self, name: &str) -> Option<u64> {
        let v = self.get(name)?;
        match v.strip_prefix("0x") {
            Some(hex) => u64::from_str_radix(hex, 16).ok(),
            None => v.parse().ok(),
        }
    }

    fn is(&self, provider: &str, id: u32) -> bool {
        self.id == id && self.provider == provider
    }
}

/// An owned event log handle.
struct Handle(EVT_HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { EvtClose(self.0) };
    }
}

pub fn read() -> Result<Boots, String> {
    let started = Instant::now();
    let window = DAYS * 86_400_000;
    let query = format!(
        "<QueryList><Query Id=\"0\">\
         <Select Path=\"System\">*[System[TimeCreated[timediff(@SystemTime) &lt;= {window}]] and \
         System[(Provider[@Name='Microsoft-Windows-Kernel-General'] and EventID=12) or \
         (Provider[@Name='Microsoft-Windows-Kernel-Boot'] and (EventID=20 or EventID=27 or EventID=30)) or \
         (Provider[@Name='Microsoft-Windows-Winlogon'] and EventID=7001) or \
         (Provider[@Name='Microsoft-Windows-Kernel-Power'] and (EventID=41 or EventID=42)) or \
         (Provider[@Name='Microsoft-Windows-Kernel-General'] and EventID=13) or \
         (Provider[@Name='User32'] and EventID=1074) or \
         (Provider[@Name='EventLog'] and EventID=6008) or \
         (Provider[@Name='Microsoft-Windows-WER-SystemErrorReporting'] and EventID=1001) or \
         (Provider[@Name='Service Control Manager'] and (EventID=7000 or EventID=7001 or \
         EventID=7009 or EventID=7011 or EventID=7022 or EventID=7023 or EventID=7024 or \
         EventID=7026 or EventID=7031 or EventID=7034 or EventID=7038))]]</Select>\
         <Select Path=\"Microsoft-Windows-Winlogon/Operational\">\
         *[System[TimeCreated[timediff(@SystemTime) &lt;= {window}]] and \
         System[EventID=1 or EventID=811 or EventID=812]]</Select>\
         <Select Path=\"Microsoft-Windows-GroupPolicy/Operational\">\
         *[System[TimeCreated[timediff(@SystemTime) &lt;= {window}]] and \
         System[EventID=8000 or EventID=8001]]</Select>\
         <Select Path=\"Microsoft-Windows-Diagnostics-Performance/Operational\">\
         *[System[TimeCreated[timediff(@SystemTime) &lt;= {window}]] and \
         System[EventID &gt;= 100 and EventID &lt;= 110]]</Select>\
         </Query></QueryList>"
    );
    let mut events = query_events(&query)?;
    // Each log comes in its own order; one time line for all.
    events.sort_by_key(|e| e.at);
    let mut boots = assemble(&events);
    boots.truncate(MAX_BOOTS);
    let note = boots
        .is_empty()
        .then(|| "The System log names no boot in the last months.".to_string());
    Ok(Boots {
        boots,
        note,
        took: started.elapsed(),
    })
}

fn query_events(query: &str) -> Result<Vec<Event>, String> {
    let q = wide(query);
    let results = unsafe {
        EvtQuery(
            None,
            PCWSTR::null(),
            PCWSTR(q.as_ptr()),
            EvtQueryChannelPath.0 | EvtQueryTolerateQueryErrors.0,
        )
    }
    .map_err(|e| format!("the event logs could not be read ({})", e.message()))?;
    let results = Handle(results);
    let mut out = Vec::new();
    let mut handles = [0isize; 64];
    loop {
        let mut got = 0u32;
        if unsafe { EvtNext(results.0, &mut handles, 1000, 0, &mut got) }.is_err() || got == 0 {
            break;
        }
        for &h in &handles[..got as usize] {
            let h = Handle(EVT_HANDLE(h));
            if let Some(e) = render(h.0).as_deref().and_then(parse) {
                out.push(e);
            }
        }
    }
    Ok(out)
}

/// The event as XML.
fn render(event: EVT_HANDLE) -> Option<String> {
    let (mut used, mut props) = (0u32, 0u32);
    // The first call says how much room it needs.
    let _ = unsafe {
        EvtRender(
            None,
            event,
            EvtRenderEventXml.0,
            0,
            None,
            &mut used,
            &mut props,
        )
    };
    if used == 0 {
        return None;
    }
    let mut buf = vec![0u16; (used as usize).div_ceil(2)];
    unsafe {
        EvtRender(
            None,
            event,
            EvtRenderEventXml.0,
            (buf.len() * 2) as u32,
            Some(buf.as_mut_ptr().cast()),
            &mut used,
            &mut props,
        )
    }
    .ok()?;
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// The text between `from` and the next `to` after it.
fn between<'a>(s: &'a str, from: &str, to: &str) -> Option<&'a str> {
    let start = s.find(from)? + from.len();
    let end = s[start..].find(to)? + start;
    Some(&s[start..end])
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The provider, id, time and named data of an event's XML.
fn parse(xml: &str) -> Option<Event> {
    let provider = between(xml, "<Provider Name='", "'")?.to_string();
    // <EventID>12</EventID>, or <EventID Qualifiers='..'>12</EventID>.
    let id_tag = xml.find("<EventID")?;
    let id = between(&xml[id_tag..], ">", "<")?.trim().parse().ok()?;
    let at = filetime_of_iso(between(xml, "<TimeCreated SystemTime='", "'")?)?;
    let mut data = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<Data Name='") {
        rest = &rest[i + "<Data Name='".len()..];
        let Some(q) = rest.find('\'') else { break };
        let name = rest[..q].to_string();
        rest = &rest[q + 1..];
        // <Data Name='x'/> is empty.
        if rest.starts_with("/>") {
            data.push((name, String::new()));
            continue;
        }
        let Some(value) = between(rest, ">", "</Data>") else {
            break;
        };
        data.push((name, unescape(value)));
    }
    let binary = between(xml, "<Binary>", "</Binary>")
        .map(|hex| {
            hex.as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .filter_map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
                .collect()
        })
        .unwrap_or_default();
    Some(Event {
        provider,
        id,
        at,
        data,
        binary,
    })
}

/// A SYSTEMTIME (UTC) as a FILETIME.
fn filetime_of_systemtime(b: &[u8]) -> Option<i64> {
    let w = |i: usize| -> Option<u16> {
        Some(u16::from_le_bytes([*b.get(i * 2)?, *b.get(i * 2 + 1)?]))
    };
    let iso = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        w(0)?,
        w(1)?,
        w(3)?,
        w(4)?,
        w(5)?,
        w(6)?,
        w(7)?
    );
    filetime_of_iso(&iso)
}

/// "%%5" (a Windows error number) in words.
fn error_words(text: &str) -> String {
    match text.strip_prefix("%%").and_then(|n| n.parse::<u32>().ok()) {
        Some(code) => windows::core::HRESULT::from_win32(code)
            .message()
            .trim()
            .trim_end_matches('.')
            .to_string(),
        None => text.trim().to_string(),
    }
}

/// A Service Control Manager event in words.
fn service_problem(e: &Event) -> Option<String> {
    let p1 = e.get("param1").unwrap_or("").trim();
    let p2 = e.get("param2").unwrap_or("").trim();
    Some(match e.id {
        7000 | 7023 => format!("{p1} did not start: {}", error_words(p2)),
        7024 => format!("{p1} stopped with its own error {p2}"),
        7001 => format!("{p1} did not start: {p2}, which it needs, did not"),
        7009 | 7011 => {
            let seconds = p1
                .parse::<u64>()
                .map_or(String::new(), |ms| format!(" within {} s", ms / 1000));
            format!("{p2} did not answer{seconds}")
        }
        7022 => format!("{p1} hung as it started"),
        7026 => {
            let drivers: Vec<&str> = p1
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            format!("Drivers did not load: {}", drivers.join(", "))
        }
        7031 | 7034 => format!("{p1} stopped unexpectedly"),
        7038 => format!("{p1} could not sign in as its account"),
        _ => return None,
    })
}

/// The program that asked for a shutdown, without the computer's name
/// after it.
fn program_of(text: &str) -> String {
    let t = text.split(" (").next().unwrap_or(text).trim();
    t.rsplit(['\\', '/']).next().unwrap_or(t).to_string()
}

/// What a Winlogon notification was about.
fn notification(n: &str) -> &'static str {
    match n {
        "0" => "session",
        "2" => "sign-in",
        "12" => "shell",
        "14" => "after the shell",
        _ => "other",
    }
}

const GENERAL: &str = "Microsoft-Windows-Kernel-General";
const KBOOT: &str = "Microsoft-Windows-Kernel-Boot";
const POWER: &str = "Microsoft-Windows-Kernel-Power";
const WINLOGON: &str = "Microsoft-Windows-Winlogon";
const DIAG: &str = "Microsoft-Windows-Diagnostics-Performance";
const USER32: &str = "User32";
const EVENTLOG: &str = "EventLog";
const WER: &str = "Microsoft-Windows-WER-SystemErrorReporting";
const SCM: &str = "Service Control Manager";
const POLICY: &str = "Microsoft-Windows-GroupPolicy";
/// A boot's own problems come in its first minutes; later ones are the
/// session's.
const STARTING: i64 = 600 * SECOND;

fn ms(ticks: i64) -> u64 {
    (ticks.max(0) / 10_000) as u64
}

/// Groups the events into boots, newest first.
fn assemble(events: &[Event]) -> Vec<Boot> {
    let mut starts: Vec<i64> = events
        .iter()
        .filter(|e| e.is(GENERAL, 12))
        .filter_map(|e| e.get("StartTime").and_then(filetime_of_iso))
        .collect();
    starts.sort_unstable();
    starts.dedup();
    let mut boots = Vec::with_capacity(starts.len());
    for (i, &start) in starts.iter().enumerate() {
        let next = starts.get(i + 1).copied().unwrap_or(i64::MAX);
        let previous = i.checked_sub(1).map_or(i64::MIN, |p| starts[p]);
        // What a boot logs about itself comes in its first minutes; a
        // little slack before, as the log's clock and the kernel's start
        // differ by a moment.
        let early =
            |e: &&Event| e.at >= start - 30 * SECOND && e.at < (start + 300 * SECOND).min(next);
        let within = |e: &&Event| e.at >= start - 30 * SECOND && e.at < next;
        let mut boot = Boot {
            started: start,
            ..Boot::default()
        };
        for e in events.iter().filter(early) {
            if e.is(KBOOT, 27) {
                boot.kind = match e.num("BootType") {
                    Some(0) => BootKind::Cold,
                    Some(1) => BootKind::FastStartup,
                    Some(2) => BootKind::Hibernation,
                    _ => BootKind::Unknown,
                };
            } else if e.is(KBOOT, 30) {
                boot.firmware_ms = e
                    .num("StartOSImageStart")
                    .filter(|&ms| ms > 0)
                    .or_else(|| e.num("LoadOSImageStart").filter(|&ms| ms > 0));
                // The loader runs from its start to handing the machine
                // over to the kernel.
                let handed = e
                    .num("ExitBootServicesExit")
                    .filter(|&ms| ms > 0)
                    .or_else(|| e.num("ExitBootServicesEntry").filter(|&ms| ms > 0));
                if let (Some(from), Some(to)) = (boot.firmware_ms, handed) {
                    boot.loader_ms = to.checked_sub(from);
                }
            } else if e.is(KBOOT, 20) && boot.before == Shutdown::Unknown {
                if e.get("LastShutdownGood") == Some("true") {
                    boot.before = Shutdown::Clean;
                } else if e.get("LastShutdownGood") == Some("false") {
                    boot.before = Shutdown::Unexpected;
                }
            }
        }
        // Kernel-Power 41 says more than "not clean".
        if let Some(e) = events.iter().filter(early).find(|e| e.is(POWER, 41)) {
            let code = e.num("BugcheckCode").unwrap_or(0);
            boot.before = if code != 0 {
                Shutdown::Crashed(code as u32)
            } else if e.num("PowerButtonTimestamp").unwrap_or(0) != 0
                || e.get("LongPowerButtonPressDetected") == Some("true")
            {
                Shutdown::PowerButton
            } else {
                Shutdown::Unexpected
            };
        }
        // The first sign-in: Winlogon's own log, or the System log's.
        let signed_in = events
            .iter()
            .filter(within)
            .filter(|e| (e.is(WINLOGON, 7001) || e.is(WINLOGON, 1)) && e.at >= start)
            .map(|e| e.at)
            .min();
        boot.ready_ms = signed_in.map(|t| ms(t - start));
        // Winlogon sets the session up as the sign-in screen comes.
        boot.to_screen_ms = events
            .iter()
            .filter(within)
            .find(|e| e.is(WINLOGON, 811) && e.at >= start)
            .map(|e| ms(e.at - start))
            .filter(|&t| boot.ready_ms.is_none_or(|r| t <= r));
        if let Some(t) = signed_in {
            let steps: Vec<&Event> = events
                .iter()
                .filter(|e| {
                    e.provider == WINLOGON && e.at >= t && e.at < (t + 180 * SECOND).min(next)
                })
                .collect();
            // The desktop is there once the step after the shell ends.
            boot.to_desktop_ms = steps
                .iter()
                .filter(|e| e.id == 812 && e.get("Event") == Some("14"))
                .map(|e| ms(e.at - t))
                .next()
                .or_else(|| {
                    steps
                        .iter()
                        .filter(|e| e.id == 812)
                        .map(|e| ms(e.at - t))
                        .max()
                });
            // Each step: from its start (811) to its end (812).
            for begin in steps.iter().filter(|e| e.id == 811) {
                let key = (begin.get("SubscriberName"), begin.get("Event"));
                if let Some(end) = steps.iter().find(|e| {
                    e.id == 812
                        && e.at >= begin.at
                        && (e.get("SubscriberName"), e.get("Event")) == key
                }) {
                    let took = ms(end.at - begin.at);
                    if took >= 200 {
                        boot.sign_in_steps.push((
                            format!(
                                "{} ({})",
                                key.0.unwrap_or("?"),
                                notification(key.1.unwrap_or(""))
                            ),
                            took,
                        ));
                    }
                }
            }
            boot.sign_in_steps
                .sort_by_key(|(_, t)| std::cmp::Reverse(*t));
            boot.policy_sign_in_ms = events
                .iter()
                .find(|e| e.is(POLICY, 8001) && e.at >= t && e.at < (t + STARTING).min(next))
                .and_then(|e| e.num("PolicyElaspedTimeInSeconds"))
                .map(|s| s * 1000);
        }
        boot.policy_start_ms = events
            .iter()
            .find(|e| e.is(POLICY, 8000) && e.at >= start && e.at < (start + STARTING).min(next))
            .and_then(|e| e.num("PolicyElaspedTimeInSeconds"))
            .map(|s| s * 1000);
        // What failed as it started, and what stopped later in the session.
        for e in events
            .iter()
            .filter(within)
            .filter(|e| e.provider == SCM && e.at >= start)
        {
            let Some(what) = service_problem(e) else {
                continue;
            };
            let problem = Problem {
                after_ms: ms(e.at - start),
                what,
            };
            if e.at < start + STARTING {
                boot.problems.push(problem);
            } else if matches!(e.id, 7031 | 7034) {
                boot.session.crashed.push(problem);
            }
        }
        boot.session.sleeps = events
            .iter()
            .filter(within)
            .filter(|e| e.is(POWER, 42) && e.at >= start)
            .count() as u32;
        // How the session before ended: who asked, and when it went down.
        let before = |e: &&Event| e.at > previous && e.at < start;
        let went_down = events
            .iter()
            .filter(before)
            .filter(|e| e.is(GENERAL, 13))
            .map(|e| e.at)
            .max();
        if let Some(asked) = events.iter().filter(before).rfind(|e| e.is(USER32, 1074)) {
            let ended = &mut boot.ended;
            ended.by = asked.get("param1").map(program_of);
            ended.action = asked.get("param5").map(str::to_string);
            ended.reason = asked
                .get("param3")
                .filter(|r| !r.starts_with("No title"))
                .map(str::to_string);
            ended.comment = asked
                .get("param6")
                .filter(|c| !c.is_empty())
                .map(str::to_string);
            ended.user = asked.get("param7").map(str::to_string);
            ended.asked_at = Some(asked.at);
            ended.shutdown_ms = went_down
                .filter(|&d| d >= asked.at)
                .map(|d| ms(d - asked.at));
        }
        boot.ended.last_alive = events
            .iter()
            .filter(early)
            .find(|e| e.is(EVENTLOG, 6008))
            .and_then(|e| e.binary.get(16..32).and_then(filetime_of_systemtime));
        let off_from = went_down.or(boot.ended.last_alive);
        boot.ended.off_ms = off_from.filter(|&t| t <= start).map(|t| ms(start - t));
        if let Some(e) = events.iter().filter(early).find(|e| e.is(WER, 1001)) {
            boot.ended.crash = e.get("param1").map(str::to_string);
            boot.ended.dump = e
                .get("param2")
                .filter(|d| !d.is_empty())
                .map(str::to_string);
        }
        for e in events.iter().filter(within).filter(|e| e.provider == DIAG) {
            match e.id {
                100 => {
                    boot.measured = Some(Measured {
                        total_ms: e.num("BootTime").unwrap_or(0),
                        main_path_ms: e.num("MainPathBootTime").unwrap_or(0),
                        after_ms: e.num("BootPostBootTime").unwrap_or(0),
                    });
                }
                101..=110 => {
                    let kind = match e.id {
                        101 => SlowKind::Program,
                        102 => SlowKind::Driver,
                        103 => SlowKind::Service,
                        109 => SlowKind::Device,
                        _ => SlowKind::System,
                    };
                    let name = e
                        .get("FriendlyName")
                        .filter(|n| !n.is_empty())
                        .or_else(|| e.get("Name"))
                        .unwrap_or("")
                        .trim_end_matches('\0')
                        .to_string();
                    boot.slowed.push(Slow {
                        kind,
                        name,
                        took_ms: e.num("TotalTime").unwrap_or(0),
                        over_ms: e.num("DegradationTime").unwrap_or(0),
                    });
                }
                _ => {}
            }
        }
        boot.slowed.sort_by_key(|s| std::cmp::Reverse(s.over_ms));
        boot.problems.sort_by_key(|p| p.after_ms);
        boot.session.crashed.sort_by_key(|p| p.after_ms);
        boots.push(boot);
    }
    // A session lasted until the next one's start, less the time off.
    for i in 0..boots.len().saturating_sub(1) {
        let next = &boots[i + 1];
        let off = next.ended.off_ms.map_or(0, |o| o as i64 * 10_000);
        boots[i].session.lasted_ms = Some(ms(next.started - off - boots[i].started));
    }
    boots.reverse();
    boots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xml(provider: &str, id: u32, at: &str, data: &[(&str, &str)]) -> String {
        let data: String = data
            .iter()
            .map(|(n, v)| {
                if v.is_empty() {
                    format!("<Data Name='{n}'/>")
                } else {
                    format!("<Data Name='{n}'>{v}</Data>")
                }
            })
            .collect();
        format!(
            "<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System>\
             <Provider Name='{provider}' Guid='{{x}}'/><EventID>{id}</EventID>\
             <TimeCreated SystemTime='{at}'/></System><EventData>{data}</EventData></Event>"
        )
    }

    #[test]
    fn reads_an_events_xml() {
        let e = parse(&xml(
            DIAG,
            101,
            "2025-10-15T15:41:44.3982976Z",
            &[
                ("Name", "nvcontainer.exe"),
                ("FriendlyName", "NVIDIA &amp; Co"),
                ("Version", ""),
                ("TotalTime", "3162"),
            ],
        ))
        .unwrap();
        assert_eq!(e.provider, DIAG);
        assert_eq!(e.id, 101);
        assert_eq!(e.get("FriendlyName"), Some("NVIDIA & Co"));
        assert_eq!(e.get("Version"), Some(""));
        assert_eq!(e.num("TotalTime"), Some(3162));
        let q = parse(
            "<Event><System><Provider Name='EventLog'/><EventID Qualifiers='32768'>6008</EventID>\
             <TimeCreated SystemTime='2026-10-30T13:50:26Z'/></System></Event>",
        )
        .unwrap();
        assert_eq!(q.id, 6008);
    }

    #[test]
    fn puts_a_boots_events_together() {
        let events: Vec<Event> = [
            xml(
                GENERAL,
                12,
                "2026-10-28T06:31:30.1Z",
                &[("StartTime", "2026-10-28T06:31:29.5Z")],
            ),
            xml(WINLOGON, 7001, "2026-10-28T06:31:54.5Z", &[("TSId", "1")]),
            xml(
                KBOOT,
                20,
                "2026-10-28T06:31:30.1Z",
                &[("LastShutdownGood", "true")],
            ),
            xml(KBOOT, 27, "2026-10-28T06:31:30.1Z", &[("BootType", "1")]),
            xml(
                GENERAL,
                12,
                "2026-10-30T13:50:23.0047428Z",
                &[("StartTime", "2026-10-30T13:50:22.5Z")],
            ),
            xml(
                KBOOT,
                20,
                "2026-10-30T13:50:23.0048680Z",
                &[("LastShutdownGood", "false")],
            ),
            xml(
                KBOOT,
                27,
                "2026-10-30T13:50:23.0049207Z",
                &[("BootType", "0")],
            ),
            xml(
                KBOOT,
                30,
                "2026-10-30T13:50:23.0145987Z",
                &[
                    ("LoadOSImageStart", "16190"),
                    ("StartOSImageStart", "16225"),
                ],
            ),
            xml(
                POWER,
                41,
                "2026-10-30T13:50:26.8Z",
                &[("BugcheckCode", "0"), ("PowerButtonTimestamp", "0")],
            ),
            xml(WINLOGON, 7001, "2026-10-30T13:50:46.5Z", &[("TSId", "1")]),
            xml(WINLOGON, 7001, "2026-10-30T15:00:00Z", &[("TSId", "1")]),
            xml(
                DIAG,
                101,
                "2026-10-30T13:52:00Z",
                &[
                    ("Name", "a.exe"),
                    ("TotalTime", "3000"),
                    ("DegradationTime", "2000"),
                ],
            ),
            xml(
                DIAG,
                103,
                "2026-10-30T13:52:00Z",
                &[
                    ("Name", "svc"),
                    ("FriendlyName", "Big Service"),
                    ("TotalTime", "9000"),
                    ("DegradationTime", "6000"),
                ],
            ),
        ]
        .iter()
        .filter_map(|x| parse(x))
        .collect();
        let boots = assemble(&events);
        assert_eq!(boots.len(), 2);
        let last = &boots[0];
        assert_eq!(
            last.started,
            filetime_of_iso("2026-10-30T13:50:22.5Z").unwrap()
        );
        assert_eq!(last.kind, BootKind::Cold);
        assert_eq!(last.firmware_ms, Some(16225));
        assert_eq!(last.before, Shutdown::Unexpected);
        assert_eq!(last.ready_ms, Some(24_000), "the first sign-in after it");
        assert_eq!(last.slowed.len(), 2);
        assert_eq!(last.slowed[0].name, "Big Service");
        assert_eq!(last.slowed[0].kind, SlowKind::Service);
        let first = &boots[1];
        assert_eq!(first.kind, BootKind::FastStartup);
        assert_eq!(first.before, Shutdown::Clean);
        assert_eq!(first.ready_ms, Some(25_000));
        assert!(first.slowed.is_empty());
    }

    #[test]
    fn names_a_stop_error() {
        let events: Vec<Event> = [
            xml(
                GENERAL,
                12,
                "2026-10-30T13:50:23Z",
                &[("StartTime", "2026-10-30T13:50:22Z")],
            ),
            xml(
                POWER,
                41,
                "2026-10-30T13:50:26Z",
                &[("BugcheckCode", "209"), ("PowerButtonTimestamp", "0")],
            ),
        ]
        .iter()
        .filter_map(|x| parse(x))
        .collect();
        assert_eq!(assemble(&events)[0].before, Shutdown::Crashed(209));
    }

    #[test]
    fn tells_how_the_session_before_ended_and_how_the_start_went() {
        let mut list = vec![
            // A session from 10:00, restarted by Windows Update at 12:00.
            xml(
                GENERAL,
                12,
                "2026-10-23T10:00:01Z",
                &[("StartTime", "2026-10-23T10:00:00Z")],
            ),
            xml(
                SCM,
                7034,
                "2026-10-23T11:00:00Z",
                &[("param1", "Everything"), ("param2", "1")],
            ),
            xml(POWER, 42, "2026-10-23T11:10:00Z", &[]),
            xml(
                USER32,
                1074,
                "2026-10-23T12:00:00Z",
                &[
                    ("param1", r"C:\Windows\servicing\TrustedInstaller.exe (PC)"),
                    ("param3", "Operating System: Upgrade (Planned)"),
                    ("param5", "restart"),
                    ("param6", ""),
                    ("param7", r"NT AUTHORITY\SYSTEM"),
                ],
            ),
            xml(GENERAL, 13, "2026-10-23T12:00:50Z", &[]),
            // The next start, 30 s after the kernel went down.
            xml(
                GENERAL,
                12,
                "2026-10-23T12:01:21Z",
                &[("StartTime", "2026-10-23T12:01:20Z")],
            ),
            xml(
                KBOOT,
                30,
                "2026-10-23T12:01:21Z",
                &[
                    ("LoadOSImageStart", "16190"),
                    ("StartOSImageStart", "16225"),
                    ("ExitBootServicesExit", "17168"),
                ],
            ),
            xml(
                SCM,
                7009,
                "2026-10-23T12:02:50Z",
                &[("param1", "90000"), ("param2", "Steam Client Service")],
            ),
            xml(
                SCM,
                7026,
                "2026-10-23T12:01:40Z",
                &[("param1", "\naehd\ndam\n")],
            ),
            xml(
                WINLOGON,
                811,
                "2026-10-23T12:01:43Z",
                &[("Event", "0"), ("SubscriberName", "GPClient")],
            ),
            xml(WINLOGON, 1, "2026-10-23T12:01:48Z", &[]),
            xml(
                WINLOGON,
                811,
                "2026-10-23T12:01:48.1Z",
                &[("Event", "2"), ("SubscriberName", "Profiles")],
            ),
            xml(
                WINLOGON,
                812,
                "2026-10-23T12:01:49.6Z",
                &[("Event", "2"), ("SubscriberName", "Profiles")],
            ),
            xml(
                WINLOGON,
                812,
                "2026-10-23T12:01:52Z",
                &[("Event", "14"), ("SubscriberName", "TermSrv")],
            ),
        ];
        // An unexpected stop: 6008 carries when it last ran, local then UTC.
        let mut lost = xml(EVENTLOG, 6008, "2026-10-23T14:00:20Z", &[]);
        lost = lost.replace(
            "</EventData>",
            "<Binary>EA070A0005001700100031001900790 1EA070A0005001700 0D00310019007901</Binary></EventData>"
                .replace(' ', "")
                .as_str(),
        );
        list.push(xml(
            GENERAL,
            12,
            "2026-10-23T14:00:01Z",
            &[("StartTime", "2026-10-23T14:00:00Z")],
        ));
        list.push(lost);
        let events: Vec<Event> = list.iter().filter_map(|x| parse(x)).collect();
        let boots = assemble(&events);
        assert_eq!(boots.len(), 3);
        let (last, middle, first) = (&boots[0], &boots[1], &boots[2]);

        let e = &middle.ended;
        assert_eq!(e.by.as_deref(), Some("TrustedInstaller.exe"));
        assert_eq!(e.action.as_deref(), Some("restart"));
        assert_eq!(
            e.reason.as_deref(),
            Some("Operating System: Upgrade (Planned)")
        );
        assert_eq!(e.comment, None);
        assert_eq!(e.shutdown_ms, Some(50_000));
        assert_eq!(e.off_ms, Some(30_000));
        assert_eq!(middle.loader_ms, Some(943));
        assert_eq!(middle.to_screen_ms, Some(23_000));
        assert_eq!(middle.ready_ms, Some(28_000));
        assert_eq!(middle.to_desktop_ms, Some(4_000));
        assert_eq!(
            middle.sign_in_steps,
            vec![("Profiles (sign-in)".to_string(), 1_500)]
        );
        assert_eq!(middle.problems.len(), 2);
        assert_eq!(middle.problems[0].what, "Drivers did not load: aehd, dam");
        assert_eq!(
            middle.problems[1].what,
            "Steam Client Service did not answer within 90 s"
        );
        assert_eq!(middle.problems[1].after_ms, 90_000);

        assert_eq!(first.session.crashed.len(), 1);
        assert_eq!(
            first.session.crashed[0].what,
            "Everything stopped unexpectedly"
        );
        assert_eq!(first.session.sleeps, 1);
        // 10:00:00 to 12:00:50, when the kernel went down.
        assert_eq!(first.session.lasted_ms, Some(7_250_000));

        assert_eq!(
            last.ended.last_alive,
            filetime_of_iso("2026-10-23T13:49:25.377Z")
        );
        assert_eq!(last.ended.by, None);
        assert_eq!(last.session.lasted_ms, None, "still running");
    }

    /// Reads this machine's boots. Some of them need an administrator.
    #[test]
    #[ignore]
    fn reads_this_machines_boots() {
        let b = read().unwrap();
        for boot in &b.boots {
            println!("{boot:?}");
        }
        println!("{} boots in {:?}, {:?}", b.boots.len(), b.took, b.note);
        assert!(!b.boots.is_empty());
    }
}
