// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Updates on Linux: the download goes through curl, which every
//! distribution ships and keeps up to date (its TLS and certificates with
//! it); the new executable replaces the old by a rename, which a running
//! program allows.
//!
//! An executable given capabilities (see `hardening::grant_command`) loses
//! them when it is replaced: they have to be granted again.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

/// The release's file for this platform, and its executable.
pub(super) const ASSET: &str = "-linux-x64.zip";
pub(super) const PLATFORM: &str = "Linux x64";
pub(super) const SYSTEM: &str = "Linux";
pub(super) const EXECUTABLE: &str = "anabolic";
pub(super) const MAGIC: &[u8] = b"\x7fELF";

/// rwxr-xr-x, as the release's executable came.
pub(super) fn make_runnable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

/// An open file never keeps a rename from happening on Linux.
pub(super) fn busy(_: &std::io::Error) -> bool {
    false
}

/// There is no installer on Linux: every copy updates from the zip.
pub(super) const PACKAGE: Option<&str> = None;

pub(super) fn installed(_exe: &Path) -> bool {
    false
}

pub(super) fn may_install_package() -> Result<(), String> {
    Ok(())
}

pub(super) fn hold(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

pub(super) fn run_package(_package: &Path, _log: &Path) -> Result<(), String> {
    Err("there is no installer on Linux".into())
}

pub(super) fn relaunch(exe: &Path, args: &[String]) -> Result<(), String> {
    Command::new(exe)
        .arg("--after")
        .arg(std::process::id().to_string())
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("the new version could not start: {e}"))
}

/// Fetches `url` (https only) with curl: the status and up to `limit`
/// bytes of the body. `headers` are "Name: value" lines separated by CRLF.
pub(super) fn get(
    url: &str,
    headers: &str,
    limit: usize,
    mut progress: impl FnMut(usize),
) -> Result<(u32, Vec<u8>), String> {
    if !url.starts_with("https://") {
        return Err(format!("not an https address: {url}"));
    }
    let host = url["https://".len()..].split('/').next().unwrap_or("");
    let mut command = Command::new("curl");
    command
        .args([
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
        ])
        .args(["--connect-timeout", "15", "--max-time", "600"])
        .args(["--max-filesize", &limit.to_string()])
        .args([
            "--user-agent",
            &format!("Anabolic/{}", env!("CARGO_PKG_VERSION")),
        ])
        // The status after the body, on a line of its own.
        .args(["--write-out", "\n%{http_code}"]);
    for header in headers.split("\r\n").filter(|h| !h.is_empty()) {
        command.args(["--header", header]);
    }
    let mut child = command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not reach {host}: curl could not start ({e})"))?;
    let mut body = Vec::new();
    let mut stdout = child.stdout.take().ok_or("curl gave no output")?;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = stdout.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
        if body.len() > limit + 16 {
            let _ = child.kill();
            return Err(format!(
                "Could not reach {host}: the answer is larger than expected"
            ));
        }
        progress(body.len());
    }
    let mut errors = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut errors);
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        let why = errors.trim().trim_start_matches("curl: ");
        return Err(format!("Could not reach {host}: {why}"));
    }
    let at = body
        .iter()
        .rposition(|&b| b == b'\n')
        .ok_or("curl's answer was cut short")?;
    let code = std::str::from_utf8(&body[at + 1..])
        .ok()
        .and_then(|c| c.trim().parse::<u32>().ok())
        .ok_or("curl gave no status")?;
    body.truncate(at);
    Ok((code, body))
}
