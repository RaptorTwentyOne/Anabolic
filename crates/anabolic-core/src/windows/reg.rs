// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The registry, as the readers here need it: a key opened in the 64-bit
//! or the 32-bit view, its values decoded (text, lists, numbers), its
//! subkeys.

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY,
    KEY_WOW64_64KEY, REG_SAM_FLAGS, RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW,
    RegQueryValueExW,
};
use windows::core::{PCWSTR, PWSTR};

use crate::util::{from_wide, wide};

/// A registry value, as much as the places read hold.
pub(crate) enum Value {
    Text(String),
    List(Vec<String>),
    Number(u64),
}

impl Value {
    pub(crate) fn text(&self) -> String {
        match self {
            Value::Text(t) => t.clone(),
            Value::List(l) => l.join(" "),
            Value::Number(n) => n.to_string(),
        }
    }
}

pub(crate) struct Key(pub(crate) HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

pub(crate) fn root_name(root: HKEY) -> &'static str {
    if root == HKEY_LOCAL_MACHINE {
        "HKLM"
    } else if root == HKEY_CURRENT_USER {
        "HKCU"
    } else if root == HKEY_CLASSES_ROOT {
        "HKCR"
    } else {
        "HKU"
    }
}

fn view(wow32: bool) -> REG_SAM_FLAGS {
    if wow32 {
        KEY_WOW64_32KEY
    } else {
        KEY_WOW64_64KEY
    }
}

pub(crate) fn open(root: HKEY, path: &str, wow32: bool) -> Option<Key> {
    let mut key = HKEY::default();
    let w = wide(path);
    let err = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(w.as_ptr()),
            None,
            KEY_READ | view(wow32),
            &mut key,
        )
    };
    (err == ERROR_SUCCESS).then_some(Key(key))
}

fn decode(kind: u32, data: &[u8]) -> Option<Value> {
    let units = || -> Vec<u16> {
        data.as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect()
    };
    match kind {
        // REG_SZ, REG_EXPAND_SZ
        1 | 2 => Some(Value::Text(from_wide(&units()))),
        // REG_MULTI_SZ
        7 => Some(Value::List(
            units()
                .split(|&c| c == 0)
                .filter(|s| !s.is_empty())
                .map(String::from_utf16_lossy)
                .collect(),
        )),
        // REG_DWORD, REG_QWORD
        4 => Some(Value::Number(
            u32::from_le_bytes(data.get(..4)?.try_into().ok()?) as u64,
        )),
        11 => Some(Value::Number(u64::from_le_bytes(
            data.get(..8)?.try_into().ok()?,
        ))),
        _ => None,
    }
}

/// Every value of a key: (name, value).
pub(crate) fn values(key: &Key) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for index in 0.. {
        let mut name = vec![0u16; 1024];
        let mut name_len = name.len() as u32;
        let mut kind = 0u32;
        let mut data = vec![0u8; 16384];
        let mut data_len = data.len() as u32;
        let err = unsafe {
            RegEnumValueW(
                key.0,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(&mut kind),
                Some(data.as_mut_ptr()),
                Some(&mut data_len),
            )
        };
        if err != ERROR_SUCCESS {
            break;
        }
        if let Some(v) = decode(kind, &data[..data_len as usize]) {
            out.push((from_wide(&name[..name_len as usize]), v));
        }
    }
    out
}

pub(crate) fn value(key: &Key, name: &str) -> Option<Value> {
    let w = wide(name);
    let mut kind = Default::default();
    let mut data = vec![0u8; 16384];
    let mut len = data.len() as u32;
    let err = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(w.as_ptr()),
            None,
            Some(&mut kind),
            Some(data.as_mut_ptr()),
            Some(&mut len),
        )
    };
    (err == ERROR_SUCCESS)
        .then(|| decode(kind.0, &data[..len as usize]))
        .flatten()
}

pub(crate) fn text(key: &Key, name: &str) -> Option<String> {
    value(key, name)
        .map(|v| v.text())
        .filter(|t| !t.trim().is_empty())
}

pub(crate) fn number(key: &Key, name: &str) -> Option<u64> {
    match value(key, name)? {
        Value::Number(n) => Some(n),
        Value::Text(t) => t.trim().parse().ok(),
        Value::List(_) => None,
    }
}

/// A number at a key's path, in the 64-bit view: the common case.
pub(crate) fn number_at(root: HKEY, path: &str, name: &str) -> Option<u64> {
    number(&open(root, path, false)?, name)
}

/// A text at a key's path, in the 64-bit view.
pub(crate) fn text_at(root: HKEY, path: &str, name: &str) -> Option<String> {
    text(&open(root, path, false)?, name)
}

pub(crate) fn subkeys(key: &Key) -> Vec<String> {
    let mut out = Vec::new();
    for index in 0.. {
        let mut name = vec![0u16; 512];
        let mut len = name.len() as u32;
        let err = unsafe {
            RegEnumKeyExW(
                key.0,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            )
        };
        if err != ERROR_SUCCESS {
            break;
        }
        out.push(from_wide(&name[..len as usize]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_known_value() {
        let root = text_at(
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            "SystemRoot",
        );
        assert!(root.is_some_and(|r| r.to_lowercase().ends_with("windows")));
        assert!(number_at(HKEY_CURRENT_USER, r"Control Panel\Mouse", "NoSuchValue").is_none());
    }
}
