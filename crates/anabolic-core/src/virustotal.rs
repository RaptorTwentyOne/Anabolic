// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! VirusTotal, by a file's SHA-256 only, in the manner of Process
//! Explorer's VirusTotal column: how many of its engines flag a file, and
//! what most call it. Only with the user's own API key, which is kept
//! encrypted for this Windows account (DPAPI), or readable by this user
//! only on Linux; never the file, only its hash. Without a key, its page
//! opens in the browser instead.

use std::path::Path;

/// The file's page on VirusTotal.
pub fn page(sha256: &str) -> String {
    format!("https://www.virustotal.com/gui/file/{sha256}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    Known {
        malicious: u32,
        suspicious: u32,
        engines: u32,
        /// What most engines that flag it call it.
        label: Option<String>,
    },
    /// No one sent VirusTotal this file.
    Unknown,
    /// Too many lookups this minute: asked again later.
    Wait,
    Failed(String),
}

impl Report {
    pub fn flagged(&self) -> bool {
        matches!(self, Report::Known { malicious, .. } if *malicious > 0)
    }

    pub fn words(&self) -> String {
        match self {
            Report::Known {
                malicious: 0,
                suspicious: 0,
                engines,
                ..
            } => format!("0 of {engines} engines flag it"),
            Report::Known {
                malicious,
                suspicious,
                engines,
                label,
            } => {
                let mut t = format!("{malicious} of {engines} engines flag it");
                if *suspicious > 0 {
                    t.push_str(&format!(", {suspicious} find it suspicious"));
                }
                if let Some(l) = label {
                    t.push_str(&format!(": {l}"));
                }
                t
            }
            Report::Unknown => "Not known to VirusTotal".into(),
            Report::Wait => "Waiting for VirusTotal".into(),
            Report::Failed(e) => format!("VirusTotal: {e}"),
        }
    }
}

/// Reads VirusTotal's answer for a file (API v3, /files/{hash}).
pub fn parse(body: &[u8]) -> Report {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Report::Failed("an answer that is not JSON".into());
    };
    let a = &v["data"]["attributes"];
    let stats = &a["last_analysis_stats"];
    let n = |k: &str| stats[k].as_u64().unwrap_or(0) as u32;
    let engines = ["malicious", "suspicious", "undetected", "harmless"]
        .iter()
        .map(|k| n(k))
        .sum();
    Report::Known {
        malicious: n("malicious"),
        suspicious: n("suspicious"),
        engines,
        label: a["popular_threat_classification"]["suggested_threat_label"]
            .as_str()
            .map(str::to_string),
    }
}

/// Asks VirusTotal about a file by its hash.
pub fn lookup(sha256: &str, key: &str) -> Report {
    if sha256.len() != 64 || !sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Report::Failed("not a SHA-256".into());
    }
    let url = format!("https://www.virustotal.com/api/v3/files/{sha256}");
    match crate::update::https_get(&url, &format!("x-apikey: {key}"), 4 << 20) {
        Ok((200, body)) => parse(&body),
        Ok((404, _)) => Report::Unknown,
        Ok((401, _)) => Report::Failed("the API key was refused".into()),
        Ok((429, _)) => Report::Wait,
        Ok((status, _)) => Report::Failed(format!("answered {status}")),
        Err(e) => Report::Failed(e),
    }
}

const KEY_FILE: &str = "virustotal.key";

/// The saved key, from Anabolic's data folder.
pub fn load_key(dir: &Path) -> Option<String> {
    let data = std::fs::read(dir.join(KEY_FILE)).ok()?;
    let key = String::from_utf8(imp::unprotect(&data)?).ok()?;
    let key = key.trim().to_string();
    (!key.is_empty()).then_some(key)
}

pub fn save_key(dir: &Path, key: &str) -> Result<(), String> {
    let data = imp::protect(key.trim().as_bytes())?;
    let path = dir.join(KEY_FILE);
    std::fs::write(&path, data).map_err(|e| e.to_string())?;
    imp::private(&path);
    Ok(())
}

pub fn forget_key(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(KEY_FILE));
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    fn take(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let out = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec();
        let _ = unsafe { LocalFree(Some(HLOCAL(blob.pbData.cast()))) };
        out
    }

    /// Encrypted for this Windows account (DPAPI).
    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr().cast_mut(),
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
        .map_err(|e| e.message())?;
        Ok(take(output))
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr().cast_mut(),
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
        .ok()?;
        Some(take(output))
    }

    pub fn private(_path: &std::path::Path) {}
}

#[cfg(target_os = "linux")]
mod imp {
    /// Linux has no per-user key store every desktop shares: the file is
    /// readable by this user only.
    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.to_vec())
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        Some(data.to_vec())
    }

    pub fn private(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_answer() {
        let body = br#"{"data":{"attributes":{"last_analysis_stats":{"malicious":3,
            "suspicious":1,"undetected":60,"harmless":0,"timeout":2},
            "popular_threat_classification":{"suggested_threat_label":"trojan.agent"}}}}"#;
        let r = parse(body);
        assert!(r.flagged());
        assert_eq!(
            r.words(),
            "3 of 64 engines flag it, 1 find it suspicious: trojan.agent"
        );
        assert_eq!(lookup("abc", "k"), Report::Failed("not a SHA-256".into()));
    }

    #[test]
    fn keeps_the_key_to_itself() {
        let dir = std::env::temp_dir().join(format!("anabolic-vt-{}", crate::random_hex(4)));
        std::fs::create_dir_all(&dir).unwrap();
        save_key(&dir, " secret-key \n").unwrap();
        let raw = std::fs::read(dir.join(KEY_FILE)).unwrap();
        if cfg!(windows) {
            assert!(
                !raw.windows(6).any(|w| w == b"secret"),
                "stored in the clear"
            );
        }
        assert_eq!(load_key(&dir).as_deref(), Some("secret-key"));
        forget_key(&dir);
        assert!(load_key(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
