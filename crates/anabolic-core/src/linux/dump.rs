//! Memory dumps (core files) of a process.

use std::path::Path;

use crate::model::ProcKey;

use super::*;

/// A name for the file: `name-pid-YYYYMMDD-HHMMSS.core`.
pub fn file_name(name: &str, pid: u32, kind: DumpKind) -> String {
    let stamp = crate::sysinfo::local_time(crate::sysinfo::now_filetime())
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>();
    let small = if kind == DumpKind::Small {
        "-small"
    } else {
        ""
    };
    format!("{name}-{pid}-{stamp}{small}.core")
}

/// The keyrings: their memory holds the session's passwords and keys.
const SIGN_IN_SECRETS: [&str; 5] = [
    "gnome-keyring-daemon",
    "kwalletd5",
    "kwalletd6",
    "ssh-agent",
    "gpg-agent",
];

/// Whether this process holds sign-in secrets (a keyring).
pub fn holds_secrets(name: &str) -> bool {
    SIGN_IN_SECRETS.contains(&name)
}

/// Whether a process of this name is not dumped: the keyrings unless
/// `secrets` (the user's setting) allows it.
pub fn refused(name: &str, secrets: bool) -> bool {
    !secrets && holds_secrets(name)
}

pub fn write(
    _key: ProcKey,
    _name: &str,
    _kind: DumpKind,
    _dir: &Path,
    _secrets: bool,
) -> Result<DumpReport, String> {
    Err("not available on Linux yet".into())
}
