// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What Explorer's right-click menu asks for (`--locks PATH`, who holds
//! it; `--space PATH`, what fills it). A start that finds Anabolic
//! already running leaves the request in a file of its data folder and
//! signals; the running one picks it up as it comes forward.

use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Locks(String),
    Space(String),
}

static QUEUE: Mutex<Vec<Request>> = Mutex::new(Vec::new());

fn file() -> Option<PathBuf> {
    crate::data_dir().map(|d| d.join("request.txt"))
}

/// The request on a command line.
pub fn from_args(args: &[String]) -> Option<Request> {
    let after = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(|p| p.trim_end_matches('"').to_string())
            .filter(|p| !p.is_empty())
    };
    after("--locks")
        .map(Request::Locks)
        .or_else(|| after("--space").map(Request::Space))
}

fn line(r: &Request) -> String {
    match r {
        Request::Locks(p) => format!("locks\t{p}"),
        Request::Space(p) => format!("space\t{p}"),
    }
}

fn parse(line: &str) -> Option<Request> {
    let (kind, path) = line.split_once('\t')?;
    match kind {
        "locks" => Some(Request::Locks(path.to_string())),
        "space" => Some(Request::Space(path.to_string())),
        _ => None,
    }
}

/// Leaves the request for whichever Anabolic serves it.
pub fn leave(r: &Request) {
    let Some(path) = file() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let temp = path.with_extension("tmp");
    if std::fs::write(&temp, line(r)).is_ok() {
        let _ = std::fs::rename(&temp, &path);
    }
}

/// Takes in a request left for this instance.
pub fn pick_up() {
    let Some(path) = file() else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let _ = std::fs::remove_file(&path);
    if let Some(r) = parse(text.trim())
        && let Ok(mut q) = QUEUE.lock()
    {
        q.push(r);
    }
}

/// What is waiting to be shown.
pub fn drain() -> Vec<Request> {
    QUEUE
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_menus_requests() {
        let args: Vec<String> = ["anabolic.exe", "--space", r#"D:\Games""#]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let r = from_args(&args).unwrap();
        // Explorer's "%V" on a drive root ends in a backslash Windows
        // turns into an escaped quote: the quote is dropped.
        assert_eq!(r, Request::Space(r"D:\Games".into()));
        assert_eq!(parse(&line(&r)), Some(r));
        assert_eq!(
            parse("locks\tC:\\a b\\c.txt"),
            Some(Request::Locks(r"C:\a b\c.txt".into()))
        );
        assert!(from_args(&["anabolic.exe".to_string()]).is_none());
    }
}
