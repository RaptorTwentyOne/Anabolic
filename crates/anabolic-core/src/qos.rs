// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Upload limits per program, the part of NetLimiter Windows can do
//! without a driver of its own: a policy-based QoS rule that holds what a
//! program sends to a rate (New-NetQosPolicy, ThrottleRate). Only what it
//! sends: holding what it receives needs a network filter driver, which
//! Anabolic does not install. The policies go in the active store, so
//! they last until Windows restarts, or until Anabolic lifts them as it
//! quits; their names start with "Anabolic: ".

pub const PREFIX: &str = "Anabolic: ";

/// A program name safe to put in a PowerShell command: a file name, no
/// quotes or separators.
pub fn safe_name(image: &str) -> bool {
    !image.is_empty()
        && image.len() <= 128
        && image
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' ' | '+'))
}

/// The PowerShell that makes the active store hold exactly these limits:
/// Anabolic's policies removed, then each wanted one made.
pub fn script(want: &[(String, u64)]) -> String {
    let mut s = format!(
        "$ErrorActionPreference = 'Stop'; Get-NetQosPolicy -PolicyStore ActiveStore | \
         Where-Object {{ $_.Name -like '{PREFIX}*' }} | \
         Remove-NetQosPolicy -PolicyStore ActiveStore -Confirm:$false;"
    );
    for (image, bits) in want.iter().filter(|(i, b)| safe_name(i) && *b > 0) {
        s.push_str(&format!(
            " New-NetQosPolicy -Name '{PREFIX}{image}' -AppPathNameMatchCondition '{image}' \
             -ThrottleRateActionBitsPerSecond {bits} -NetworkProfile All \
             -PolicyStore ActiveStore | Out-Null;"
        ));
    }
    s
}

/// Makes the limits these and only these; says what holds now.
#[cfg(windows)]
pub fn sync(want: &[(String, u64)]) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let shell = format!(r"{root}\System32\WindowsPowerShell\v1.0\powershell.exe");
    let out = std::process::Command::new(shell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script(want),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("PowerShell could not start: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        let first = why
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("it failed");
        return Err(if first.to_lowercase().contains("access") {
            "Only an administrator may set upload limits".into()
        } else {
            first.to_string()
        });
    }
    let kept: Vec<&(String, u64)> = want
        .iter()
        .filter(|(i, b)| safe_name(i) && *b > 0)
        .collect();
    Ok(if kept.is_empty() {
        "No upload limit is held now.".into()
    } else {
        format!(
            "Upload limited for {}.",
            kept.iter()
                .map(|(i, b)| format!("{i} ({})", words(*b)))
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

#[cfg(target_os = "linux")]
pub fn sync(want: &[(String, u64)]) -> Result<String, String> {
    if want.is_empty() {
        Ok("No upload limit is held now.".into())
    } else {
        Err(
            "Upload limits per program need tc and cgroups on Linux; Anabolic does not set them."
                .into(),
        )
    }
}

/// "8 Mbit/s (1 MB/s)".
pub fn words(bits: u64) -> String {
    let mb = bits as f64 / 8.0 / 1_000_000.0;
    format!("{} MB/s", (mb * 100.0).round() / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_only_safe_names() {
        assert!(safe_name("chrome.exe"));
        assert!(!safe_name("a';Remove-Item C:\\ -Recurse;'"));
        let s = script(&[("steam.exe".into(), 8_000_000), ("x'; evil".into(), 1000)]);
        assert!(s.contains("-Name 'Anabolic: steam.exe' -AppPathNameMatchCondition 'steam.exe'"));
        assert!(s.contains("-ThrottleRateActionBitsPerSecond 8000000"));
        assert!(!s.contains("evil"));
        assert_eq!(words(8_000_000), "1 MB/s");
    }
}
