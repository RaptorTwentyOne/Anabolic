// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Crashes from systemd-coredump: every program that stopped on a signal
//! that leaves a core (a segmentation fault, an abort), with the signal and
//! whether its core is still kept. `coredumpctl` reads its journal.

use std::process::Command;
use std::time::Instant;

use crate::crashes::{Crash, CrashKind, Crashes};

/// From 1601 to 1970, in 100 ns.
const UNIX_TO_FILETIME: i64 = 116_444_736_000_000_000;

pub fn read() -> Result<Crashes, String> {
    let started = Instant::now();
    let out = Command::new("coredumpctl")
        .args(["list", "--json=short", "--no-pager", "--no-legend"])
        .output();
    let mut notes = Vec::new();
    let crashes = match out {
        Ok(o) if o.status.success() => parse(&String::from_utf8_lossy(&o.stdout)),
        // With none recorded, coredumpctl says so and fails.
        Ok(o) if String::from_utf8_lossy(&o.stderr).contains("No coredumps found") => Vec::new(),
        Ok(o) => {
            notes.push(format!(
                "coredumpctl could not list the crashes: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ));
            Vec::new()
        }
        Err(_) => {
            notes.push(
                "systemd-coredump is not installed, so crashes are not recorded on this system."
                    .into(),
            );
            Vec::new()
        }
    };
    Ok(Crashes {
        crashes,
        notes,
        since: None,
        took: started.elapsed(),
    })
}

/// coredumpctl's JSON: `[{"time":1730000000000000,"pid":1234,"sig":11,
/// "corefile":"present","exe":"/usr/bin/x",...}]`, oldest first.
fn parse(json: &str) -> Vec<Crash> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(json)
    else {
        return Vec::new();
    };
    let mut out: Vec<Crash> = items
        .iter()
        .filter_map(|i| {
            let micros = i.get("time")?.as_i64()?;
            let exe = i
                .get("exe")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let program = exe.rsplit('/').next().unwrap_or(&exe).to_string();
            let kept = i.get("corefile").and_then(|v| v.as_str()) == Some("present");
            Some(Crash {
                at: micros * 10 + UNIX_TO_FILETIME,
                kind: CrashKind::Crash,
                program: if program.is_empty() {
                    "A program".into()
                } else {
                    program
                },
                path: (!exe.is_empty()).then_some(exe),
                code: i.get("sig").and_then(|v| v.as_u64()).map(|s| s as u32),
                pid: i.get("pid").and_then(|v| v.as_u64()).map(|p| p as u32),
                detail: Some(
                    if kept {
                        "its core is kept: coredumpctl debug opens it"
                    } else {
                        "its core is no longer kept"
                    }
                    .to_string(),
                ),
                ..Crash::default()
            })
        })
        .collect();
    out.sort_by_key(|c| std::cmp::Reverse(c.at));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_coredumpctls_list() {
        let list = parse(
            r#"[{"time":1731400000000000,"pid":4242,"uid":1000,"gid":1000,"sig":11,
                "corefile":"present","exe":"/usr/bin/foo","size":12345},
               {"time":1731500000000000,"pid":77,"sig":6,"corefile":"missing",
                "exe":"/opt/bar/bar"}]"#,
        );
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].program, "bar");
        assert_eq!(list[0].code, Some(6));
        assert_eq!(list[1].pid, Some(4242));
        assert!(list[1].detail.as_deref().unwrap().contains("kept"));
    }
}
