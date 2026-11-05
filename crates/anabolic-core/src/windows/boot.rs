// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Boots from Windows' event logs. The System log has, for every boot, the
//! kernel's start (Kernel-General 12), the firmware's timings (Kernel-Boot
//! 30), the kind of start (Kernel-Boot 27), whether the last shutdown was
//! clean (Kernel-Boot 20, Kernel-Power 41) and the sign-ins (Winlogon 7001).
//! Diagnostics-Performance adds Windows' own measurement and what slowed a
//! boot, on the machines where it still keeps them.

use std::time::Instant;

use windows::Win32::System::EventLog::{
    EVT_HANDLE, EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EvtQueryTolerateQueryErrors,
    EvtRender, EvtRenderEventXml,
};
use windows::core::PCWSTR;

use crate::boot::{Boot, BootKind, Boots, Measured, Shutdown, Slow, SlowKind, filetime_of_iso};
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
         (Provider[@Name='Microsoft-Windows-Kernel-Power'] and EventID=41)]]</Select>\
         <Select Path=\"Microsoft-Windows-Diagnostics-Performance/Operational\">\
         *[System[TimeCreated[timediff(@SystemTime) &lt;= {window}]] and \
         System[EventID &gt;= 100 and EventID &lt;= 110]]</Select>\
         </Query></QueryList>"
    );
    let events = query_events(&query)?;
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
    Some(Event {
        provider,
        id,
        at,
        data,
    })
}

const GENERAL: &str = "Microsoft-Windows-Kernel-General";
const KBOOT: &str = "Microsoft-Windows-Kernel-Boot";
const POWER: &str = "Microsoft-Windows-Kernel-Power";
const WINLOGON: &str = "Microsoft-Windows-Winlogon";
const DIAG: &str = "Microsoft-Windows-Diagnostics-Performance";

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
        boot.ready_ms = events
            .iter()
            .filter(within)
            .filter(|e| e.is(WINLOGON, 7001) && e.at >= start)
            .map(|e| ((e.at - start) / 10_000) as u64)
            .min();
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
        boots.push(boot);
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
