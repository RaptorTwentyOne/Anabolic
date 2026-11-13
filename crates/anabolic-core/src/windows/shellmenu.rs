// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Explorer's right-click menu, in the manner of LockHunter: "Who is using
//! this?" on files and folders, "What fills this folder?" on folders and
//! drives. Verbs under the user's own Classes key (no administrator
//! needed, nothing machine-wide); on Windows 11 they are under "Show more
//! options". Each runs Anabolic with --locks or --space and the path.

use windows::Win32::System::Registry::HKEY_CURRENT_USER;

use crate::reg;

/// Class, verb, what the menu says, the option, the path placeholder.
const VERBS: [(&str, &str, &str, &str, &str); 5] = [
    (
        "*",
        "AnabolicLocks",
        "Who is using this? (Anabolic)",
        "--locks",
        "%1",
    ),
    (
        "Directory",
        "AnabolicLocks",
        "Who is using this folder? (Anabolic)",
        "--locks",
        "%1",
    ),
    (
        "Directory",
        "AnabolicSpace",
        "What fills this folder? (Anabolic)",
        "--space",
        "%1",
    ),
    (
        r"Directory\Background",
        "AnabolicSpace",
        "What fills this folder? (Anabolic)",
        "--space",
        "%V",
    ),
    (
        "Drive",
        "AnabolicSpace",
        "What fills this drive? (Anabolic)",
        "--space",
        "%1",
    ),
];

const CLASSES: &str = r"Software\Classes";

fn key(base: &str, class: &str, verb: &str) -> String {
    format!(r"{base}\{class}\shell\{verb}")
}

fn register_at(base: &str, exe: &str) -> Result<(), String> {
    for (class, verb, label, option, placeholder) in VERBS {
        let k = key(base, class, verb);
        reg::set_text(HKEY_CURRENT_USER, &k, "MUIVerb", label)?;
        reg::set_text(HKEY_CURRENT_USER, &k, "Icon", &format!("\"{exe}\",0"))?;
        reg::set_text(
            HKEY_CURRENT_USER,
            &format!(r"{k}\command"),
            "",
            &format!("\"{exe}\" {option} \"{placeholder}\""),
        )?;
    }
    Ok(())
}

fn unregister_at(base: &str) -> Result<(), String> {
    for (class, verb, ..) in VERBS {
        reg::delete_key(HKEY_CURRENT_USER, &key(base, class, verb))?;
    }
    Ok(())
}

/// The program the menu runs now, when it is there.
fn registered_at(base: &str) -> Option<String> {
    let (class, verb, ..) = VERBS[0];
    let command = reg::text_at(
        HKEY_CURRENT_USER,
        &format!(r"{}\command", key(base, class, verb)),
        "",
    )?;
    command
        .strip_prefix('"')
        .and_then(|c| c.split('"').next())
        .map(str::to_string)
}

/// Puts the verbs in Explorer's menu, running `exe`.
pub fn register(exe: &str) -> Result<(), String> {
    register_at(CLASSES, exe)
}

pub fn unregister() -> Result<(), String> {
    unregister_at(CLASSES)
}

pub fn registered() -> Option<String> {
    registered_at(CLASSES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn puts_the_verbs_in_and_takes_them_out() {
        let base = format!(r"Software\Anabolic-test-{}", crate::random_hex(4));
        register_at(&base, r"C:\Tools\anabolic.exe").unwrap();
        assert_eq!(
            registered_at(&base).as_deref(),
            Some(r"C:\Tools\anabolic.exe")
        );
        let command = reg::text_at(
            HKEY_CURRENT_USER,
            &format!(r"{base}\Directory\Background\shell\AnabolicSpace\command"),
            "",
        );
        assert_eq!(
            command.as_deref(),
            Some(r#""C:\Tools\anabolic.exe" --space "%V""#)
        );
        unregister_at(&base).unwrap();
        assert!(registered_at(&base).is_none());
        reg::delete_key(HKEY_CURRENT_USER, &base).unwrap();
    }
}
