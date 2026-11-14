// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Crashes from Windows' own records. The Application log has each crash
//! (Application Error 1000: the program, the module, the exception and
//! where in the module), each hang that ended a program (Application Hang
//! 1002), a .NET program's unhandled exception with its stack (.NET Runtime
//! 1026), and Windows Error Reporting's events (1001), of which these are
//! used: its resource exhaustion detector's verdicts that a program leaks
//! (RADAR_PRE_LEAK), programs that stopped responding a while
//! (AppHangTransient) and the drivers it had to reset (LiveKernelEvent).
//! The System log has display driver resets (Display 4101) and stop errors
//! (WER-SystemErrorReporting 1001). Error Reporting's report folders reach
//! further back than the logs and keep its dumps; local crash dumps go to
//! a folder of their own.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RegGetValueW};
use windows::core::w;

use crate::crashes::{Crash, CrashKind, Crashes, live_event_words, stop_words};
use crate::evtlog::{Event, oldest, query};
use crate::util::from_wide;

const SECOND: i64 = 10_000_000;

pub fn read() -> Result<Crashes, String> {
    let started = Instant::now();
    let list = "<QueryList><Query Id='0'>\
        <Select Path='Application'>*[System[\
        (Provider[@Name='Application Error'] and EventID=1000) or \
        (Provider[@Name='Application Hang'] and EventID=1002) or \
        (Provider[@Name='.NET Runtime'] and EventID=1026) or \
        (Provider[@Name='Windows Error Reporting'] and EventID=1001)]]</Select>\
        <Select Path='System'>*[System[\
        (Provider[@Name='Display'] and EventID=4101) or \
        (Provider[@Name='Microsoft-Windows-WER-SystemErrorReporting'] and EventID=1001)]]</Select>\
        </Query></QueryList>";
    let events = query(list)?;
    let mut notes = Vec::new();
    let reports = reports(&mut notes);
    let dumps = local_dumps();
    let since = oldest("Application");
    let mut crashes = assemble(&events, &reports, &dumps, since);
    crashes.sort_by_key(|c| std::cmp::Reverse(c.at));
    // Error Reporting keeps reports from before the log's start: the
    // records reach back to the older of the two.
    let since = match (since, crashes.last().map(|c| c.at)) {
        (Some(s), Some(oldest)) => Some(s.min(oldest)),
        (s, oldest) => s.or(oldest),
    };
    Ok(Crashes {
        crashes,
        notes,
        since,
        took: started.elapsed(),
    })
}

/// What Windows Error Reporting kept about one problem: its Report.wer.
#[derive(Debug, Default, Clone)]
pub(crate) struct Report {
    folder: PathBuf,
    kind: String,
    /// As a FILETIME.
    at: i64,
    id: String,
    integrator_id: String,
    app_path: Option<String>,
    /// Its signature: "Application Name" = "chrome.exe" and so on.
    sigs: Vec<(String, String)>,
    /// A dump in its folder.
    dump: Option<PathBuf>,
}

impl Report {
    fn sig(&self, name: &str) -> Option<&str> {
        self.sigs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    }
}

/// A Report.wer: UTF-16 key=value lines.
pub(crate) fn parse_report(text: &str, folder: &Path) -> Option<Report> {
    let mut r = Report {
        folder: folder.to_path_buf(),
        ..Report::default()
    };
    let mut names: HashMap<String, String> = HashMap::new();
    let mut values: HashMap<String, String> = HashMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.trim_start_matches('\u{feff}').split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key {
            "EventType" => r.kind = value.to_string(),
            "EventTime" => r.at = value.parse().unwrap_or(0),
            "ReportIdentifier" => r.id = value.to_string(),
            "IntegratorReportIdentifier" => r.integrator_id = value.to_string(),
            "AppPath" => r.app_path = Some(value.to_string()).filter(|v| !v.is_empty()),
            _ => {
                // Sig[3].Name / Sig[3].Value
                if let Some(rest) = key.strip_prefix("Sig[")
                    && let Some((n, field)) = rest.split_once("].")
                {
                    match field {
                        "Name" => names.insert(n.to_string(), value.to_string()),
                        "Value" => values.insert(n.to_string(), value.to_string()),
                        _ => None,
                    };
                }
            }
        }
    }
    if r.kind.is_empty() || r.at == 0 {
        return None;
    }
    let mut keys: Vec<&String> = names.keys().collect();
    keys.sort_by_key(|k| k.parse::<u32>().unwrap_or(u32::MAX));
    r.sigs = keys
        .into_iter()
        .map(|k| (names[k].clone(), values.get(k).cloned().unwrap_or_default()))
        .collect();
    Some(r)
}

/// Every report in Error Reporting's folders, the machine's and this
/// user's.
fn reports(notes: &mut Vec<String>) -> Vec<Report> {
    let mut roots = vec![PathBuf::from(r"C:\ProgramData\Microsoft\Windows\WER")];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        roots.push(PathBuf::from(local).join(r"Microsoft\Windows\WER"));
    }
    let mut out = Vec::new();
    for root in roots {
        for sub in ["ReportArchive", "ReportQueue"] {
            let dir = root.join(sub);
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    let note = "Error Reporting's machine-wide reports need administrator \
                                rights; older crashes may be missing."
                        .to_string();
                    if !notes.contains(&note) {
                        notes.push(note);
                    }
                    continue;
                }
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let folder = entry.path();
                let Ok(bytes) = std::fs::read(folder.join("Report.wer")) else {
                    continue;
                };
                let units: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| u16::from_le_bytes(*b))
                    .collect();
                let Some(mut r) = parse_report(&String::from_utf16_lossy(&units), &folder) else {
                    continue;
                };
                r.dump = std::fs::read_dir(&folder).ok().and_then(|files| {
                    files.flatten().map(|f| f.path()).find(|p| {
                        p.extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| ["dmp", "mdmp", "hdmp"].contains(&e))
                    })
                });
                out.push(r);
            }
        }
    }
    out
}

/// The dumps Windows wrote itself (LocalDumps), by program file name in
/// lower case and process id: `chrome.exe.161364.dmp`.
fn local_dumps() -> HashMap<(String, u32), PathBuf> {
    let mut folders = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        folders.push(PathBuf::from(local).join("CrashDumps"));
    }
    if let Some(folder) = dump_folder() {
        folders.push(folder);
    }
    let mut out = HashMap::new();
    for dir in folders {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for path in entries.flatten().map(|e| e.path()) {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".dmp") else {
                continue;
            };
            let Some((program, pid)) = stem.rsplit_once('.') else {
                continue;
            };
            if let Ok(pid) = pid.parse() {
                out.insert((program.to_ascii_lowercase(), pid), path);
            }
        }
    }
    out
}

/// The machine's LocalDumps folder, when one is set.
fn dump_folder() -> Option<PathBuf> {
    let mut buf = [0u16; 520];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps"),
            w!("DumpFolder"),
            RRF_RT_REG_EXPAND_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    status
        .is_ok()
        .then(|| PathBuf::from(from_wide(&buf)))
        .filter(|p| !p.as_os_str().is_empty())
}

fn hex(v: &str) -> Option<u64> {
    let v = v.trim();
    let v = v.strip_prefix("0x").unwrap_or(v);
    u64::from_str_radix(v, 16).ok()
}

/// A value that says nothing: Windows' placeholders for "not known".
fn known(v: Option<&str>) -> Option<String> {
    let v = v?.trim();
    // StackHash_e344: no module, only a hash of the stack.
    (!v.is_empty()
        && !v.starts_with("StackHash_")
        && !matches!(v, "unknown" | "bad_module_info" | "0.0.0.0" | "00000000"))
    .then(|| v.to_string())
}

/// A Store app's name from its package: 5319275A.WhatsAppDesktop_2.26…
/// is WhatsAppDesktop.
fn package_name(package: &str) -> String {
    let name = package.split('_').next().unwrap_or(package);
    name.split_once('.')
        .map_or(name, |(_, rest)| rest)
        .to_string()
}

/// The program's file name in a path.
fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

fn ms(ticks: i64) -> Option<u64> {
    (ticks > 0).then_some((ticks / 10_000) as u64)
}

fn assemble(
    events: &[Event],
    reports: &[Report],
    dumps: &HashMap<(String, u32), PathBuf>,
    since: Option<i64>,
) -> Vec<Crash> {
    let report_of = |id: Option<&str>| -> Option<&Report> {
        let id = id?.trim();
        (!id.is_empty())
            .then(|| {
                reports.iter().find(|r| {
                    r.id.eq_ignore_ascii_case(id) || r.integrator_id.eq_ignore_ascii_case(id)
                })
            })
            .flatten()
    };
    let mut used: Vec<&Path> = Vec::new();
    let mut out: Vec<Crash> = Vec::new();
    let mut dotnet: Vec<&Event> = Vec::new();
    for e in events {
        let mut c = Crash {
            at: e.at,
            ..Crash::default()
        };
        match (e.provider.as_str(), e.id) {
            ("Application Error", 1000) => {
                let package = known(e.get("PackageFullName"));
                c.kind = CrashKind::Crash;
                c.path = known(e.get("AppPath"));
                c.program = known(e.get("AppName"))
                    .or_else(|| package.as_deref().map(package_name))
                    .unwrap_or_else(|| "A program".into());
                c.package = package;
                c.version = known(e.get("AppVersion"));
                c.module = known(e.get("ModuleName"));
                c.module_path = known(e.get("ModulePath"));
                c.module_version = known(e.get("ModuleVersion"));
                c.code = e
                    .get("ExceptionCode")
                    .and_then(hex)
                    .filter(|&v| v != 0)
                    .map(|v| v as u32);
                c.offset = e.get("FaultingOffset").and_then(hex);
                c.pid = e.get("ProcessId").and_then(hex).map(|v| v as u32);
                c.ran_ms = e
                    .get("ProcessCreationTime")
                    .and_then(hex)
                    .and_then(|t| ms(e.at - t as i64));
                let report =
                    report_of(e.get("IntegratorReportId")).or_else(|| report_of(e.get("ReportId")));
                if let Some(r) = report {
                    used.push(&r.folder);
                    c.report = Some(r.folder.display().to_string());
                    c.dump = r.dump.as_ref().map(|d| d.display().to_string());
                }
            }
            ("Application Hang", 1002) => {
                let package = known(e.get("PackageFullName"));
                c.kind = CrashKind::Hang;
                c.path = known(e.get("ExeFileName"));
                c.program = known(e.get("AppName"))
                    .or_else(|| package.as_deref().map(package_name))
                    .unwrap_or_else(|| "A program".into());
                c.package = package;
                c.version = known(e.get("AppVersion"));
                c.pid = e.get("ProcessId").and_then(hex).map(|v| v as u32);
                c.ran_ms = e
                    .get("StartTime")
                    .and_then(hex)
                    .and_then(|t| ms(e.at - t as i64));
                c.detail = match e.get("HangType").map(str::trim) {
                    Some("Cross-process") => Some("it was waiting for another process".to_string()),
                    Some("Quiesce") => Some("it was closing down".to_string()),
                    _ => None,
                };
                if let Some(r) = report_of(e.get("ReportId")) {
                    used.push(&r.folder);
                    c.report = Some(r.folder.display().to_string());
                    c.dump = r.dump.as_ref().map(|d| d.display().to_string());
                }
            }
            (".NET Runtime", 1026) => {
                dotnet.push(e);
                continue;
            }
            ("Windows Error Reporting", 1001) => {
                let p = |i: u32| known(e.get(&format!("P{i}")));
                match e.get("EventName").map(str::trim).unwrap_or("") {
                    name if name.starts_with("RADAR_PRE_LEAK") => {
                        c.kind = CrashKind::Leak;
                        c.program = p(1).unwrap_or_else(|| "A program".into());
                        c.version = p(2);
                    }
                    "AppHangTransient" => {
                        c.kind = CrashKind::Stall;
                        c.program = p(1).unwrap_or_else(|| "A program".into());
                        c.version = p(2);
                    }
                    "LiveKernelEvent" => {
                        c.kind = CrashKind::DriverReset;
                        c.program = "Windows".into();
                        c.code = p(1).as_deref().and_then(hex).map(|v| v as u32);
                        c.detail = c.code.map(|code| match live_event_words(code) {
                            Some(words) => words.to_string(),
                            None => format!("live kernel event 0x{code:X}"),
                        });
                        c.dump = attached_dump(e.get("AttachedFiles"));
                    }
                    _ => continue,
                }
                if let Some(folder) = e.get("StorePath") {
                    let folder = folder.trim().trim_start_matches(r"\\?\");
                    if let Some(r) = reports.iter().find(|r| r.folder == Path::new(folder)) {
                        used.push(&r.folder);
                        c.report = Some(folder.to_string());
                        if c.dump.is_none() {
                            c.dump = r.dump.as_ref().map(|d| d.display().to_string());
                        }
                    }
                }
            }
            ("Display", 4101) => {
                c.kind = CrashKind::DriverReset;
                let driver = e
                    .data
                    .first()
                    .map(|(_, v)| v.trim().to_string())
                    .filter(|v| !v.is_empty());
                c.program = driver
                    .as_deref()
                    .map_or_else(|| "Display driver".into(), |d| format!("{d}.sys"));
                c.module.clone_from(&driver);
                c.detail = Some(
                    "the display driver stopped responding and was reset; the screen went black \
                     for a moment"
                        .into(),
                );
            }
            ("Microsoft-Windows-WER-SystemErrorReporting", 1001) => {
                c.kind = CrashKind::StopError;
                c.program = "Windows".into();
                let p1 = e.get("param1").unwrap_or("").trim();
                c.code = p1.split_whitespace().next().and_then(hex).map(|v| v as u32);
                c.detail = Some(match c.code.and_then(stop_words) {
                    Some(name) => format!("{name}: {p1}"),
                    None => p1.to_string(),
                });
                c.dump = known(e.get("param2"));
            }
            _ => continue,
        }
        if c.dump.is_none()
            && let Some(pid) = c.pid
        {
            let key = (file_name(&c.program).to_ascii_lowercase(), pid);
            c.dump = dumps.get(&key).map(|p| p.display().to_string());
        }
        out.push(c);
    }

    // A .NET program's exception goes with its crash, which follows it.
    for e in dotnet {
        let text = e.data.first().map(|(_, v)| v.as_str()).unwrap_or("");
        let program = text
            .lines()
            .find_map(|l| l.strip_prefix("Application:"))
            .map(|p| p.trim().to_string())
            .unwrap_or_else(|| "A .NET program".into());
        let detail = dotnet_detail(text);
        let near = out.iter_mut().find(|c| {
            c.kind == CrashKind::Crash
                && c.program.eq_ignore_ascii_case(&program)
                && (c.at - e.at).abs() <= 60 * SECOND
                && c.detail.is_none()
        });
        match near {
            Some(c) => c.detail = Some(detail),
            None => out.push(Crash {
                at: e.at,
                kind: CrashKind::Crash,
                program,
                code: Some(0xE043_4352),
                detail: Some(detail),
                ..Crash::default()
            }),
        }
    }

    // Reports no event names: older than the logs, or from a log that was
    // cleared. One the logs do cover is left out when a crash of the same
    // program lies within seconds of it.
    for r in reports {
        if used.contains(&r.folder.as_path()) {
            continue;
        }
        let Some(mut c) = from_report(r) else {
            continue;
        };
        let covered = since.is_some_and(|s| r.at >= s)
            && out.iter().any(|o| {
                o.program.eq_ignore_ascii_case(&c.program) && (o.at - r.at).abs() <= 10 * SECOND
            });
        if covered {
            continue;
        }
        c.report = Some(r.folder.display().to_string());
        c.dump = r.dump.as_ref().map(|d| d.display().to_string());
        out.push(c);
    }
    out
}

/// A crash from a report alone.
fn from_report(r: &Report) -> Option<Crash> {
    let kind = match r.kind.as_str() {
        "APPCRASH" | "BEX" | "BEX64" | "MoAppCrash" | "CLR20r3" => CrashKind::Crash,
        "AppHangB1" | "AppHangXProcB1" | "MoAppHang" | "MoAppHangXProc" => CrashKind::Hang,
        "AppHangTransient" => CrashKind::Stall,
        k if k.starts_with("RADAR_PRE_LEAK") => CrashKind::Leak,
        _ => return None,
    };
    let package = known(r.sig("Package Full Name"));
    let program = known(r.sig("Application Name"))
        .filter(|n| !n.starts_with("praid:"))
        .or_else(|| known(r.sig("Problem Signature 01")))
        .or_else(|| r.app_path.as_deref().map(file_name))
        .or_else(|| package.as_deref().map(package_name))?;
    let mut c = Crash {
        at: r.at,
        kind,
        program,
        path: r.app_path.clone(),
        package,
        version: known(r.sig("Application Version"))
            .or_else(|| known(r.sig("Problem Signature 02"))),
        module: known(r.sig("Fault Module Name")),
        module_version: known(r.sig("Fault Module Version")),
        code: r
            .sig("Exception Code")
            .and_then(hex)
            .filter(|&v| v != 0)
            .map(|v| v as u32),
        offset: r.sig("Exception Offset").and_then(hex),
        ..Crash::default()
    };
    if r.kind == "CLR20r3" {
        c.code = Some(0xE043_4352);
        c.module = known(r.sig("Problem Signature 04"));
        c.detail = known(r.sig("Problem Signature 09"));
    }
    Some(c)
}

/// A .NET crash's words: the exception and the first frames of its stack.
fn dotnet_detail(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Application:")
                && !l.starts_with("CoreCLR Version:")
                && !l.starts_with(".NET Version:")
                && !l.starts_with("Framework Version:")
                // Every unhandled exception says so.
                && !l.starts_with("Description: The process was terminated due to an unhandled")
        })
        .map(|l| l.strip_prefix("Exception Info: ").unwrap_or(l))
        .take(12)
        .collect();
    lines.join("\n")
}

/// The dump among an event's attached files.
fn attached_dump(files: Option<&str>) -> Option<String> {
    files?
        .lines()
        .map(|l| l.trim().trim_start_matches(r"\\?\"))
        .find(|l| l.to_ascii_lowercase().ends_with(".dmp") && Path::new(l).exists())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evtlog::parse;

    fn xml(provider: &str, id: u32, at: &str, data: &[(&str, &str)]) -> String {
        let data: String = data
            .iter()
            .map(|(n, v)| {
                if n.is_empty() {
                    format!("<Data>{v}</Data>")
                } else {
                    format!("<Data Name='{n}'>{v}</Data>")
                }
            })
            .collect();
        format!(
            "<Event><System><Provider Name='{provider}'/><EventID>{id}</EventID>\
             <TimeCreated SystemTime='{at}'/></System><EventData>{data}</EventData></Event>"
        )
    }

    #[test]
    fn reads_a_crash_a_hang_and_a_dotnet_exception() {
        let created = crate::boot::filetime_of_iso("2026-11-10T15:40:58Z").unwrap();
        let created = format!("0x{created:x}");
        let events: Vec<Event> = [
            xml(
                ".NET Runtime",
                1026,
                "2026-11-10T15:44:57Z",
                &[(
                    "",
                    "Application: dotnet.exe\nCoreCLR Version: 11.0\nDescription: The process \
                     was terminated due to an unhandled exception.\nException Info: \
                     System.IO.FileNotFoundException: no such file\n   at Program.Main()",
                )],
            ),
            xml(
                "Application Error",
                1000,
                "2026-11-10T15:44:58Z",
                &[
                    ("AppName", "dotnet.exe"),
                    ("AppVersion", "11.0.26.53018"),
                    ("ModuleName", "KERNELBASE.dll"),
                    ("ModuleVersion", "10.0.26100.9711"),
                    ("ExceptionCode", "e0434352"),
                    ("FaultingOffset", "00000000000c408a"),
                    ("ProcessId", "0x1f40"),
                    ("ProcessCreationTime", created.as_str()),
                    ("AppPath", r"C:\Program Files\dotnet\dotnet.exe"),
                    ("ModulePath", r"C:\Windows\System32\KERNELBASE.dll"),
                    ("IntegratorReportId", "a331e111"),
                ],
            ),
            xml(
                "Application Hang",
                1002,
                "2026-11-09T11:08:55Z",
                &[
                    ("AppName", "reti-devtools.exe"),
                    ("AppVersion", "0.0.0.0"),
                    ("ProcessId", "0xce28"),
                    ("ExeFileName", r"D:\Reti\reti-devtools.exe"),
                    ("HangType", "Cross-process"),
                ],
            ),
            xml(
                "Windows Error Reporting",
                1001,
                "2026-11-11T18:26:56Z",
                &[
                    ("EventName", "RADAR_PRE_LEAK_64"),
                    ("P1", "anabolic.exe"),
                    ("P2", "0.7.1.0"),
                ],
            ),
            xml(
                "Windows Error Reporting",
                1001,
                "2026-11-11T20:44:14Z",
                &[("EventName", "APPCRASH"), ("P1", "chrome.exe")],
            ),
        ]
        .iter()
        .filter_map(|x| parse(x))
        .collect();
        let report = Report {
            folder: PathBuf::from(r"C:\WER\AppCrash_dotnet"),
            kind: "APPCRASH".into(),
            at: events[1].at,
            integrator_id: "a331e111".into(),
            dump: Some(PathBuf::from(r"C:\WER\AppCrash_dotnet\memory.hdmp")),
            ..Report::default()
        };
        let mut dumps = HashMap::new();
        dumps.insert(
            ("reti-devtools.exe".to_string(), 0xce28),
            PathBuf::from(r"C:\Dumps\reti-devtools.exe.52776.dmp"),
        );
        let list = assemble(&events, &[report], &dumps, Some(0));
        // The APPCRASH report event duplicates Application Error's: left out.
        assert_eq!(list.len(), 3);

        let crash = &list[0];
        assert_eq!(crash.kind, CrashKind::Crash);
        assert_eq!(crash.program, "dotnet.exe");
        assert_eq!(crash.module.as_deref(), Some("KERNELBASE.dll"));
        assert_eq!(crash.code, Some(0xE043_4352));
        assert_eq!(crash.pid, Some(0x1f40));
        assert_eq!(crash.ran_ms, Some(240_000));
        assert_eq!(
            crash.dump.as_deref(),
            Some(r"C:\WER\AppCrash_dotnet\memory.hdmp")
        );
        let detail = crash.detail.as_deref().unwrap();
        assert!(detail.starts_with("System.IO.FileNotFoundException: no such file"));
        assert!(detail.contains("at Program.Main()"));

        let hang = &list[1];
        assert_eq!(hang.kind, CrashKind::Hang);
        assert_eq!(hang.version, None);
        assert_eq!(
            hang.detail.as_deref(),
            Some("it was waiting for another process")
        );
        assert_eq!(
            hang.dump.as_deref(),
            Some(r"C:\Dumps\reti-devtools.exe.52776.dmp")
        );

        let leak = &list[2];
        assert_eq!(leak.kind, CrashKind::Leak);
        assert_eq!(leak.program, "anabolic.exe");
        assert_eq!(leak.version.as_deref(), Some("0.7.1.0"));
    }

    #[test]
    fn reads_a_report_and_keeps_one_the_logs_no_longer_have() {
        let text = "\u{feff}Version=1\r\nEventType=APPCRASH\r\nEventTime=134390397901552468\r\n\
                    ReportIdentifier=d8670a85\r\nSig[0].Name=Application Name\r\n\
                    Sig[0].Value=steamwebhelper.exe\r\nSig[1].Name=Application Version\r\n\
                    Sig[1].Value=11.15.70.27\r\nSig[3].Name=Fault Module Name\r\n\
                    Sig[3].Value=libcef.dll\r\nSig[6].Name=Exception Code\r\n\
                    Sig[6].Value=c0000409\r\nAppPath=C:\\Steam\\steamwebhelper.exe\r\n";
        let r = parse_report(text, Path::new(r"C:\WER\x")).unwrap();
        assert_eq!(r.kind, "APPCRASH");
        assert_eq!(r.sig("Fault Module Name"), Some("libcef.dll"));
        // The logs start after it: it is shown from the report.
        let list = assemble(&[], &[r], &HashMap::new(), Some(134_390_397_901_552_469));
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].program, "steamwebhelper.exe");
        assert_eq!(list[0].code, Some(0xC000_0409));
        assert_eq!(list[0].module.as_deref(), Some("libcef.dll"));
        assert_eq!(list[0].report.as_deref(), Some(r"C:\WER\x"));
    }

    #[test]
    fn names_store_apps_by_their_package() {
        assert_eq!(
            package_name("5319275A.WhatsAppDesktop_2.2642.101.0_x64__cv1g1gvanyjgm"),
            "WhatsAppDesktop"
        );
    }

    #[test]
    #[ignore = "reads this machine's logs"]
    fn reads_this_machines_crashes() {
        let c = read().unwrap();
        for x in c.crashes.iter().take(40) {
            println!(
                "{:?} {} {:?} {:?} {:?} {:?}",
                x.kind,
                x.program,
                x.module,
                x.code.map(|c| format!("{c:08x}")),
                x.dump,
                x.detail.as_deref().map(|d| d.lines().next().unwrap_or(""))
            );
        }
        println!("{} in {:?}; notes {:?}", c.crashes.len(), c.took, c.notes);
    }
}
