// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The speed test's file on Linux: opened past the page cache (O_DIRECT),
//! one descriptor per request in flight, and unlinked once all
//! are open, so it goes when they close.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;

pub fn create(path: &Path, size: u64) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_DIRECT)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.set_len(size).map_err(|e| e.to_string())?;
    Ok(file)
}

pub fn open(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_DIRECT)
        .open(path)
        .map_err(|e| e.to_string())
}

/// Removes the name; the data goes when the last descriptor closes.
pub fn forget(path: &Path) {
    let _ = std::fs::remove_file(path);
}

pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    file.read_exact_at(buf, offset)
}

pub fn write_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    file.write_all_at(buf, offset)
}
