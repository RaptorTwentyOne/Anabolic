// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The speed test's file on Windows: opened past the cache
//! (FILE_FLAG_NO_BUFFERING, as CrystalDiskMark does), one handle per
//! request in flight (a synchronous handle does one thing at a time), the
//! first deleting the file when it closes, whatever happens to Anabolic.

use std::fs::{File, OpenOptions};
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::path::Path;

const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
/// FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE.
const SHARE_ALL: u32 = 7;

pub fn create(path: &Path, size: u64) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(SHARE_ALL)
        .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_DELETE_ON_CLOSE)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.set_len(size).map_err(|e| e.to_string())?;
    Ok(file)
}

pub fn open(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(SHARE_ALL)
        .custom_flags(FILE_FLAG_NO_BUFFERING)
        .open(path)
        .map_err(|e| e.to_string())
}

/// Windows deletes it when the first handle closes.
pub fn forget(_path: &Path) {}

pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        match file.seek_read(&mut buf[done..], offset + done as u64)? {
            0 => break,
            n => done += n,
        }
    }
    Ok(())
}

pub fn write_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        done += file.seek_write(&buf[done..], offset + done as u64)?;
    }
    Ok(())
}
