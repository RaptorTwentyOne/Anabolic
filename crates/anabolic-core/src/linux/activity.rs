//! Recording what processes do, on Linux.

use super::*;

impl ActivityEvent {
    /// The result in a word, as Process Monitor puts it.
    pub fn result_text(&self) -> String {
        match (self.kind, self.status) {
            (_, None) => String::new(),
            (ActivityKind::ProcessExit, Some(code)) => format!("exit code {code}"),
            (_, Some(status)) => status_text(status),
        }
    }

    /// `result_text` when it is a fixed phrase.
    pub fn result_word(&self) -> Option<&'static str> {
        match (self.kind, self.status) {
            (ActivityKind::ProcessExit, _) | (_, None) => None,
            (_, Some(status)) => status_word(status),
        }
    }

    /// An errno other than 0.
    pub fn failed(&self) -> bool {
        self.kind != ActivityKind::ProcessExit && self.status.is_some_and(|s| s != 0)
    }
}

/// An errno in words; the number itself for the rare ones.
pub fn status_text(status: u32) -> String {
    status_word(status)
        .map(str::to_string)
        .unwrap_or_else(|| format!("error {status}"))
}

/// An errno in words, when it has them.
pub fn status_word(status: u32) -> Option<&'static str> {
    Some(match status as i32 {
        0 => "success",
        libc::ENOENT => "no such file",
        libc::EACCES => "access denied",
        libc::EPERM => "not permitted",
        libc::EEXIST => "already exists",
        libc::EISDIR => "is a directory",
        libc::ENOTDIR => "not a directory",
        libc::ENOTEMPTY => "directory not empty",
        libc::EBUSY => "busy",
        libc::EAGAIN => "try again",
        libc::EINVAL => "invalid argument",
        libc::ECONNREFUSED => "connection refused",
        libc::ECONNRESET => "connection reset",
        libc::ETIMEDOUT => "timed out",
        libc::EINPROGRESS => "in progress",
        _ => return None,
    })
}

/// Records what a set of processes does.
pub struct ActivityRecorder {
    _private: (),
}

impl ActivityRecorder {
    pub fn attach(_pid: u32) -> Result<Self, String> {
        Err("not available on Linux yet".into())
    }

    pub fn everything() -> Result<Self, String> {
        Err("not available on Linux yet".into())
    }

    pub fn attach_all(_pids: &[u32]) -> Result<Self, String> {
        Err("not available on Linux yet".into())
    }

    pub fn launch(program: &str, args: &str, dir: Option<&str>) -> Result<Self, String> {
        Self::launch_with(program, args, dir, LaunchHow::Hidden)
    }

    pub fn launch_with(
        _program: &str,
        _args: &str,
        _dir: Option<&str>,
        _how: LaunchHow,
    ) -> Result<Self, String> {
        Err("not available on Linux yet".into())
    }

    pub fn take(&mut self) -> Vec<ActivityEvent> {
        Vec::new()
    }

    pub fn summary(&self) -> ActivitySummary {
        ActivitySummary::default()
    }

    pub fn tracked(&self) -> Vec<u32> {
        Vec::new()
    }

    pub fn state(&self) -> RecorderState {
        RecorderState::Failed("not available on Linux yet".into())
    }

    pub fn track(&self, _pid: u32) {}

    pub fn stop(self) -> Vec<ActivityEvent> {
        Vec::new()
    }
}
