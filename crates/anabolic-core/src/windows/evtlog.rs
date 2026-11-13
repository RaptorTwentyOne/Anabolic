// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Windows' event logs, read with a structured XML query: each event's
//! provider, id, time and named data, as much as Boots and Crashes use.

use windows::Win32::System::EventLog::{
    EVT_HANDLE, EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EvtQueryTolerateQueryErrors,
    EvtRender, EvtRenderEventXml,
};
use windows::core::PCWSTR;

use crate::boot::filetime_of_iso;
use crate::util::wide;

/// One event, as much of it as is used.
#[derive(Debug, Default)]
pub(crate) struct Event {
    pub provider: String,
    pub id: u32,
    /// When it was logged, as a FILETIME.
    pub at: i64,
    pub data: Vec<(String, String)>,
    /// The event's binary data.
    pub binary: Vec<u8>,
}

impl Event {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.data
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn num(&self, name: &str) -> Option<u64> {
        let v = self.get(name)?;
        match v.strip_prefix("0x") {
            Some(hex) => u64::from_str_radix(hex, 16).ok(),
            None => v.parse().ok(),
        }
    }

    pub fn is(&self, provider: &str, id: u32) -> bool {
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

/// The events a structured XML query (a QueryList) selects, in each
/// log's own order.
pub(crate) fn query(query: &str) -> Result<Vec<Event>, String> {
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

/// When a log's oldest event was written, as a FILETIME: how far back the
/// log reaches.
pub(crate) fn oldest(channel: &str) -> Option<i64> {
    let (path, all) = (wide(channel), wide("*"));
    let results = unsafe {
        EvtQuery(
            None,
            PCWSTR(path.as_ptr()),
            PCWSTR(all.as_ptr()),
            EvtQueryChannelPath.0,
        )
    }
    .ok()?;
    let results = Handle(results);
    let mut handles = [0isize; 1];
    let mut got = 0u32;
    unsafe { EvtNext(results.0, &mut handles, 1000, 0, &mut got) }.ok()?;
    if got == 0 {
        return None;
    }
    let event = Handle(EVT_HANDLE(handles[0]));
    parse(&render(event.0)?).map(|e| e.at)
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
pub(crate) fn between<'a>(s: &'a str, from: &str, to: &str) -> Option<&'a str> {
    let start = s.find(from)? + from.len();
    let end = s[start..].find(to)? + start;
    Some(&s[start..end])
}

pub(crate) fn unescape(s: &str) -> String {
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
pub(crate) fn parse(xml: &str) -> Option<Event> {
    let provider = between(xml, "<Provider Name='", "'")?.to_string();
    // <EventID>12</EventID>, or <EventID Qualifiers='..'>12</EventID>.
    let id_tag = xml.find("<EventID")?;
    let id = between(&xml[id_tag..], ">", "<")?.trim().parse().ok()?;
    let at = filetime_of_iso(between(xml, "<TimeCreated SystemTime='", "'")?)?;
    let mut data = Vec::new();
    let mut rest = xml;
    // <Data Name='x'>…</Data>, or a classic event's <Data>…</Data> without
    // a name.
    while let Some(i) = rest.find("<Data") {
        rest = &rest[i + "<Data".len()..];
        let name = if let Some(named) = rest.strip_prefix(" Name='") {
            let Some(q) = named.find('\'') else { break };
            let name = named[..q].to_string();
            rest = &named[q + 1..];
            name
        } else if rest.starts_with('>') || rest.starts_with("/>") {
            String::new()
        } else {
            // <DataItem> and the like.
            continue;
        };
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
