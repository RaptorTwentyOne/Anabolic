// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The NTFS master file table, read straight from the volume: every file's
//! names, folder, sizes and last write in one pass over the table, as
//! WizTree does, instead of asking for each folder.
//!
//! The table is read in large unbuffered pieces (the file cache is left
//! alone) on several threads, each with its own handle, and each piece is
//! parsed by the thread that read it. A disk that seeks gets two readers,
//! one that does not gets up to eight. Needs the right to read the volume,
//! which administrators have.

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use windows::Win32::Foundation::{GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_NO_BUFFERING, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING, ReadFile,
};
use windows::Win32::System::IO::{DeviceIoControl, OVERLAPPED, OVERLAPPED_0, OVERLAPPED_0_0};
use windows::Win32::System::Ioctl::{
    DEVICE_SEEK_PENALTY_DESCRIPTOR, FSCTL_GET_NTFS_VOLUME_DATA, IOCTL_STORAGE_QUERY_PROPERTY,
    NTFS_VOLUME_DATA_BUFFER, PropertyStandardQuery, STORAGE_PROPERTY_QUERY,
    StorageDeviceSeekPenaltyProperty,
};
use windows::core::PCWSTR;

use crate::space::{
    Builder, CANCELLED, DIR, Entry, HIDDEN, LINK, META, NONE, ORPHANS, Progress, ROOT, SYSTEM,
};
use crate::util::{OwnedHandle, wide};

/// The root folder's record.
pub const ROOT_RECORD: u64 = 5;
/// How much is read at once.
const PIECE: usize = 4 << 20;

const FILE_SIG: u32 = u32::from_le_bytes(*b"FILE");
const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_INDEX_ALLOCATION: u32 = 0xA0;
const ATTR_END: u32 = 0xFFFF_FFFF;
/// The short (8.3) name, which every long name may have beside it.
const NAMESPACE_DOS: u8 = 2;
const COMPRESSED: u16 = 0x0001;
const SPARSE: u16 = 0x8000;
/// Records below this are NTFS's own files.
const FIRST_USER_RECORD: u32 = 24;
const REF_MASK: u64 = 0xFFFF_FFFF_FFFF;

/// An open volume and its layout.
struct Volume {
    device: Vec<u16>,
    handle: OwnedHandle,
    record: usize,
    cluster: u64,
    data: NTFS_VOLUME_DATA_BUFFER,
}

fn open_device(device: &[u16]) -> Result<OwnedHandle, String> {
    unsafe {
        CreateFileW(
            PCWSTR(device.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_NO_BUFFERING,
            None,
        )
    }
    .map(OwnedHandle)
    .map_err(|e| format!("the volume could not be opened: {}", e.message()))
}

impl Volume {
    /// `device` is the volume's name without the trailing backslash, such as
    /// `\\?\Volume{...}` or `\\.\C:`.
    fn open(device: &str) -> Result<Volume, String> {
        let device = wide(device);
        let handle = open_device(&device)?;
        let mut data = NTFS_VOLUME_DATA_BUFFER::default();
        let mut got = 0u32;
        unsafe {
            DeviceIoControl(
                handle.raw(),
                FSCTL_GET_NTFS_VOLUME_DATA,
                None,
                0,
                Some((&raw mut data).cast()),
                size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
                Some(&mut got),
                None,
            )
        }
        .map_err(|e| format!("not an NTFS volume ({})", e.message()))?;
        let record = data.BytesPerFileRecordSegment as usize;
        let cluster = u64::from(data.BytesPerCluster);
        if !(512..=65536).contains(&record)
            || !record.is_multiple_of(512)
            || cluster == 0
            || record as u64 > cluster
        {
            return Err("the volume reports an unusual layout".into());
        }
        Ok(Volume {
            device,
            handle,
            record,
            cluster,
            data,
        })
    }

    /// Whether the disk under the volume seeks (a hard disk).
    fn seeks(&self) -> bool {
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceSeekPenaltyProperty,
            QueryType: PropertyStandardQuery,
            ..Default::default()
        };
        let mut out = DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
        let mut got = 0u32;
        let asked = unsafe {
            DeviceIoControl(
                self.handle.raw(),
                IOCTL_STORAGE_QUERY_PROPERTY,
                Some((&raw const query).cast()),
                size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                Some((&raw mut out).cast()),
                size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
                Some(&mut got),
                None,
            )
        };
        // Unknown counts as seeking: fewer readers never hurt a disk.
        asked.is_err() || out.IncursSeekPenalty
    }

    /// The table's pieces on the disk as (byte offset, bytes), in order.
    fn extents(&self) -> Result<Vec<(u64, u64)>, String> {
        let start = self.data.MftStartLcn as u64 * self.cluster;
        let first = self.read_records(start, 1)?;
        let mut pieces = Vec::new();
        let mut list = None;
        attributes(&first, |kind, a| match kind {
            ATTR_DATA if a.len() > 0x40 && a[8] != 0 && a[9] == 0 => {
                pieces.push((u64_at(a, 0x10), a.to_vec()));
            }
            ATTR_ATTRIBUTE_LIST => list = Some(a.to_vec()),
            _ => {}
        });
        let decode = |pieces: &[(u64, Vec<u8>)]| -> Result<Vec<(u64, u64)>, String> {
            let mut out = Vec::new();
            for (_, a) in pieces {
                let at = u16_at(a, 0x20) as usize;
                let runs = runs(a.get(at..).unwrap_or(&[])).ok_or("unreadable table layout")?;
                for (lcn, clusters) in runs {
                    let lcn = lcn.ok_or("the table has a hole")?;
                    out.push((lcn as u64 * self.cluster, clusters * self.cluster));
                }
            }
            Ok(out)
        };
        // A fragmented table keeps the rest of its layout in other records,
        // named by its attribute list, which a very fragmented one keeps
        // outside its record too.
        if let Some(list) = list {
            let value = if list[8] == 0 {
                resident_value(&list)
                    .ok_or("unreadable attribute list")?
                    .to_vec()
            } else {
                let at = u16_at(&list, 0x20) as usize;
                let size = u64_at(&list, 0x30) as usize;
                let mut value = Vec::with_capacity(size);
                for (lcn, clusters) in
                    runs(list.get(at..).unwrap_or(&[])).ok_or("unreadable attribute list")?
                {
                    let lcn = lcn.ok_or("the attribute list has a hole")?;
                    value.extend(self.read_bytes(
                        lcn as u64 * self.cluster,
                        (clusters * self.cluster) as usize,
                    )?);
                }
                value.truncate(size);
                value
            };
            let mut at = 0;
            while at + 0x1A <= value.len() {
                let kind = u32_at(&value, at);
                let len = u16_at(&value, at + 4) as usize;
                let named = value[at + 6] != 0;
                let segment = u64_at(&value, at + 0x10) & REF_MASK;
                if len == 0 {
                    break;
                }
                if kind == ATTR_DATA && !named && segment != 0 {
                    pieces.sort_by_key(|(vcn, _)| *vcn);
                    let known = decode(&pieces)?;
                    let offset = byte_in(&known, segment * self.record as u64)
                        .ok_or("the table's layout record is out of reach")?;
                    let rec = self.read_records(offset, 1)?;
                    attributes(&rec, |kind, a| {
                        if kind == ATTR_DATA && a.len() > 0x40 && a[8] != 0 && a[9] == 0 {
                            pieces.push((u64_at(a, 0x10), a.to_vec()));
                        }
                    });
                }
                at += len;
            }
            pieces.sort_by_key(|(vcn, _)| *vcn);
            pieces.dedup_by_key(|(vcn, _)| *vcn);
        }
        decode(&pieces)
    }

    /// Reads `len` bytes at any offset, through aligned unbuffered reads.
    fn read_bytes(&self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        let start = offset & !4095;
        let skip = (offset - start) as usize;
        let mut buf = Aligned::new((skip + len).next_multiple_of(4096));
        read_at(self.handle.raw(), start, buf.bytes())?;
        Ok(buf.bytes()[skip..skip + len].to_vec())
    }

    /// Reads `count` records at a byte offset, fixed up.
    fn read_records(&self, offset: u64, count: usize) -> Result<Vec<u8>, String> {
        let mut out = self.read_bytes(offset, self.record * count)?;
        for rec in out.chunks_mut(self.record) {
            if u32_at(rec, 0) != FILE_SIG || !fixup(rec) {
                return Err("a table record is damaged".into());
            }
        }
        Ok(out)
    }
}

/// Where a byte of the table lies on the disk.
fn byte_in(extents: &[(u64, u64)], mut at: u64) -> Option<u64> {
    for &(offset, len) in extents {
        if at < len {
            return Some(offset + at);
        }
        at -= len;
    }
    None
}

/// Memory aligned for unbuffered reads.
struct Aligned {
    ptr: *mut u8,
    layout: Layout,
}

// It is owned by one thread at a time.
unsafe impl Send for Aligned {}

impl Aligned {
    fn new(len: usize) -> Aligned {
        let layout = Layout::from_size_align(len.max(4096), 4096).expect("a valid layout");
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            handle_alloc_error(layout);
        }
        Aligned { ptr, layout }
    }

    fn bytes(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }
}

impl Drop for Aligned {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

fn read_at(handle: HANDLE, offset: u64, buf: &mut [u8]) -> Result<(), String> {
    let mut ov = OVERLAPPED {
        Anonymous: OVERLAPPED_0 {
            Anonymous: OVERLAPPED_0_0 {
                Offset: offset as u32,
                OffsetHigh: (offset >> 32) as u32,
            },
        },
        ..Default::default()
    };
    let mut read = 0u32;
    unsafe { ReadFile(handle, Some(buf), Some(&mut read), Some(&mut ov)) }
        .map_err(|e| format!("reading the table failed: {}", e.message()))?;
    if (read as usize) < buf.len() {
        return Err("the table ended early".into());
    }
    Ok(())
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    b.get(at..at + 2)
        .map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .map_or(0, |s| u32::from_le_bytes(s.try_into().unwrap()))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8)
        .map_or(0, |s| u64::from_le_bytes(s.try_into().unwrap()))
}

/// Puts back the last two bytes of each 512-byte stride, which NTFS swaps
/// for a check value on disk; false when a check value does not match (a
/// record torn by a crash).
fn fixup(rec: &mut [u8]) -> bool {
    let at = u16_at(rec, 4) as usize;
    let count = u16_at(rec, 6) as usize;
    if count < 2 || at + count * 2 > rec.len() {
        return false;
    }
    let check = [rec[at], rec[at + 1]];
    for i in 1..count {
        let end = i * 512;
        if end > rec.len() {
            return false;
        }
        if rec[end - 2..end] != check {
            return false;
        }
        rec[end - 2] = rec[at + 2 * i];
        rec[end - 1] = rec[at + 2 * i + 1];
    }
    true
}

/// Calls `each(type, attribute)` for every attribute of a fixed-up record.
fn attributes(rec: &[u8], mut each: impl FnMut(u32, &[u8])) {
    let used = (u32_at(rec, 0x18) as usize).min(rec.len());
    let mut at = u16_at(rec, 0x14) as usize;
    while at + 16 <= used {
        let kind = u32_at(rec, at);
        if kind == ATTR_END {
            break;
        }
        let len = u32_at(rec, at + 4) as usize;
        if len < 16 || at + len > used {
            break;
        }
        each(kind, &rec[at..at + len]);
        at += len;
    }
}

/// A resident attribute's value.
fn resident_value(a: &[u8]) -> Option<&[u8]> {
    if a.get(8) != Some(&0) {
        return None;
    }
    let len = u32_at(a, 0x10) as usize;
    let at = u16_at(a, 0x14) as usize;
    a.get(at..at + len)
}

fn le(b: &[u8], signed: bool) -> i64 {
    let mut v: i64 = 0;
    for (i, &byte) in b.iter().enumerate() {
        v |= i64::from(byte) << (8 * i);
    }
    let bits = 8 * b.len();
    if signed && bits < 64 && b.last().is_some_and(|&x| x & 0x80 != 0) {
        v -= 1 << bits;
    }
    v
}

/// Decodes a data run list into (first cluster, clusters); `None` for a
/// hole.
fn runs(b: &[u8]) -> Option<Vec<(Option<i64>, u64)>> {
    let mut out = Vec::new();
    let mut at = 0;
    let mut lcn: i64 = 0;
    while at < b.len() {
        let head = b[at];
        if head == 0 {
            break;
        }
        let len_size = usize::from(head & 0x0F);
        let off_size = usize::from(head >> 4);
        at += 1;
        if len_size == 0 || len_size > 8 || off_size > 8 || at + len_size + off_size > b.len() {
            return None;
        }
        let len = le(&b[at..at + len_size], false) as u64;
        at += len_size;
        if off_size == 0 {
            out.push((None, len));
        } else {
            lcn += le(&b[at..at + off_size], true);
            at += off_size;
            out.push((Some(lcn), len));
        }
    }
    Some(out)
}

/// What one in-use record says.
#[derive(Clone, Copy, Default)]
struct Rec {
    number: u32,
    seq: u16,
    dir: bool,
    /// The record this one extends; NONE for a file's own record.
    base: u32,
    size: u64,
    has_size: bool,
    alloc: u64,
    modified: i64,
    attrs: u32,
    names_at: u32,
    names: u16,
}

#[derive(Clone, Copy)]
struct Name {
    parent: u64,
    namespace: u8,
    at: u32,
    len: u32,
}

/// The records of one piece of the table.
#[derive(Default)]
struct Parsed {
    recs: Vec<Rec>,
    names: Vec<Name>,
    text: String,
}

fn filetime_to_unix(ft: u64) -> i64 {
    if ft == 0 {
        0
    } else {
        (ft as i64 - 116_444_736_000_000_000) / 10_000_000
    }
}

fn parse(rec: &mut [u8], number: u32, cluster: u64, out: &mut Parsed) {
    if u32_at(rec, 0) != FILE_SIG || !fixup(rec) {
        return;
    }
    let flags = u16_at(rec, 0x16);
    if flags & 1 == 0 {
        return;
    }
    let base = u64_at(rec, 0x20) & REF_MASK;
    let mut r = Rec {
        number,
        seq: u16_at(rec, 0x10),
        dir: flags & 2 != 0,
        base: if base == 0 { NONE } else { base as u32 },
        names_at: out.names.len() as u32,
        ..Rec::default()
    };
    attributes(rec, |kind, a| {
        let resident = a[8] == 0;
        let unnamed = a[9] == 0;
        match kind {
            ATTR_STANDARD_INFORMATION => {
                if let Some(v) = resident_value(a)
                    && v.len() >= 0x24
                {
                    r.modified = filetime_to_unix(u64_at(v, 0x08));
                    r.attrs = u32_at(v, 0x20);
                }
            }
            ATTR_FILE_NAME => {
                let Some(v) = resident_value(a) else {
                    return;
                };
                let chars = usize::from(*v.get(0x40).unwrap_or(&0));
                let Some(raw) = v.get(0x42..0x42 + chars * 2) else {
                    return;
                };
                let at = out.text.len() as u32;
                let units = raw
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]));
                out.text.extend(
                    char::decode_utf16(units).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)),
                );
                out.names.push(Name {
                    parent: u64_at(v, 0),
                    namespace: v[0x41],
                    at,
                    len: out.text.len() as u32 - at,
                });
                r.names += 1;
            }
            ATTR_DATA if resident => {
                if unnamed && let Some(v) = resident_value(a) {
                    r.size = v.len() as u64;
                    r.has_size = true;
                }
            }
            ATTR_DATA | ATTR_INDEX_ALLOCATION if !resident && a.len() >= 0x40 => {
                // Only the piece holding the first cluster carries the sizes.
                if u64_at(a, 0x10) != 0 {
                    return;
                }
                let aflags = u16_at(a, 0x0C);
                r.alloc += if aflags & (COMPRESSED | SPARSE) != 0 && a.len() >= 0x48 {
                    u64_at(a, 0x40)
                } else if !unnamed {
                    // A named stream can be all holes without saying so:
                    // $BadClus's covers the whole volume and uses nothing.
                    runs(a.get(u16_at(a, 0x20) as usize..).unwrap_or(&[])).map_or(0, |runs| {
                        runs.iter()
                            .filter(|r| r.0.is_some())
                            .map(|r| r.1 * cluster)
                            .sum()
                    })
                } else {
                    u64_at(a, 0x28)
                };
                if kind == ATTR_DATA && unnamed {
                    r.size = u64_at(a, 0x30);
                    r.has_size = true;
                }
            }
            _ => {}
        }
    });
    out.recs.push(r);
}

/// Reads the whole table of the volume at `device` and returns the tree
/// under `root_record` (the volume's root folder, or the record of the
/// folder that was asked for), named `root_name`.
pub fn scan(
    device: &str,
    root_record: u64,
    root_name: &str,
    progress: &Progress,
) -> Result<Builder, String> {
    let volume = Volume::open(device)?;
    let extents = volume.extents()?;
    let record = volume.record;
    let total = (volume.data.MftValidDataLength as u64 / record as u64) as usize;
    if total == 0 || root_record as usize >= total {
        return Err("the table is empty".into());
    }

    // The pieces to read: (disk offset, bytes, first record).
    let per_piece = (PIECE / record) * record;
    let mut pieces = Vec::new();
    let mut first = 0usize;
    for &(offset, len) in &extents {
        let mut done = 0u64;
        while done < len && first < total {
            let want = ((len - done) as usize).min(per_piece);
            let records = (want / record).min(total - first);
            let bytes = (records * record).next_multiple_of(4096).min(want);
            if records == 0 {
                break;
            }
            pieces.push((offset + done, bytes, first as u32));
            first += records;
            done += want as u64;
        }
    }
    progress
        .table_total
        .store(pieces.iter().map(|p| p.1 as u64).sum(), Ordering::Relaxed);

    // A few readers keep the disk busiest; more of them only get in each
    // other's way (measured on NVMe: 2.2 GB/s with four, half with eight).
    // A disk that seeks gets one. The records are parsed by other threads.
    let readers = if volume.seeks() { 1 } else { 4 }.min(pieces.len()).max(1);
    let parsers = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(2, 16);
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let error = Mutex::new(None::<String>);
    let parsed = Mutex::new(Vec::<Parsed>::with_capacity(pieces.len()));
    let (full_tx, full_rx) = crossbeam_channel::bounded::<(Aligned, usize)>(parsers);
    let (free_tx, free_rx) = crossbeam_channel::unbounded::<Aligned>();
    for _ in 0..readers + parsers + 2 {
        let _ = free_tx.send(Aligned::new(per_piece.next_multiple_of(4096)));
    }
    let fail = |e: String| {
        failed.store(true, Ordering::Relaxed);
        error.lock().unwrap().get_or_insert(e);
    };
    std::thread::scope(|s| {
        for _ in 0..readers {
            let full_tx = full_tx.clone();
            let free_rx = free_rx.clone();
            let (next, failed, fail, pieces) = (&next, &failed, &fail, &pieces);
            let device = &volume.device;
            s.spawn(move || {
                crate::hardening::set_current_thread_priority(
                    crate::hardening::ThreadPriority::BelowNormal,
                );
                let handle = match open_device(device) {
                    Ok(h) => h,
                    Err(e) => return fail(e),
                };
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= pieces.len() || failed.load(Ordering::Relaxed) || progress.cancelled() {
                        break;
                    }
                    let Ok(mut buf) = free_rx.recv() else {
                        break;
                    };
                    let (offset, bytes, _) = pieces[i];
                    if let Err(e) = read_at(handle.raw(), offset, &mut buf.bytes()[..bytes]) {
                        return fail(e);
                    }
                    progress
                        .table_read
                        .fetch_add(bytes as u64, Ordering::Relaxed);
                    if full_tx.send((buf, i)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(full_tx);
        for _ in 0..parsers {
            let full_rx = full_rx.clone();
            let free_tx = free_tx.clone();
            let (parsed, pieces) = (&parsed, &pieces);
            let cluster = volume.cluster;
            s.spawn(move || {
                crate::hardening::set_current_thread_priority(
                    crate::hardening::ThreadPriority::BelowNormal,
                );
                while let Ok((mut buf, i)) = full_rx.recv() {
                    let (_, bytes, first) = pieces[i];
                    let mut out = Parsed::default();
                    let records = (bytes / record).min(total - first as usize);
                    for (k, rec) in buf.bytes()[..records * record]
                        .chunks_exact_mut(record)
                        .enumerate()
                    {
                        parse(rec, first + k as u32, cluster, &mut out);
                    }
                    let _ = free_tx.send(buf);
                    let (mut files, mut folders, mut size) = (0, 0, 0);
                    for r in out.recs.iter().filter(|r| r.base == NONE) {
                        if r.dir {
                            folders += 1;
                        } else {
                            files += 1;
                            size += r.size;
                        }
                    }
                    progress.add(files, folders, size);
                    parsed.lock().unwrap().push(out);
                }
            });
        }
        drop(full_rx);
    });
    if progress.cancelled() {
        return Err(CANCELLED.into());
    }
    if let Some(e) = error.into_inner().unwrap() {
        return Err(e);
    }
    Ok(build(
        parsed.into_inner().unwrap(),
        total,
        root_record as u32,
        root_name,
    ))
}

/// Joins the pieces into one tree under `root`.
fn build(pieces: Vec<Parsed>, total: usize, root: u32, root_name: &str) -> Builder {
    // One list of records and names.
    let mut recs = Vec::with_capacity(pieces.iter().map(|p| p.recs.len()).sum());
    let mut names = Vec::with_capacity(pieces.iter().map(|p| p.names.len()).sum());
    let mut text = String::with_capacity(pieces.iter().map(|p| p.text.len()).sum());
    for p in pieces {
        let (name_base, text_base) = (names.len() as u32, text.len() as u32);
        recs.extend(p.recs.into_iter().map(|mut r| {
            r.names_at += name_base;
            r
        }));
        names.extend(p.names.into_iter().map(|mut n| {
            n.at += text_base;
            n
        }));
        text.push_str(&p.text);
    }

    let mut rec_of = vec![NONE; total];
    for (i, r) in recs.iter().enumerate() {
        if r.base == NONE && (r.number as usize) < total {
            rec_of[r.number as usize] = i as u32;
        }
    }
    // A record too full for all its attributes continues in others.
    let mut more: HashMap<u32, Vec<(u32, u16)>> = HashMap::new();
    for i in 0..recs.len() {
        let r = recs[i];
        if r.base == NONE || r.base as usize >= total {
            continue;
        }
        let b = rec_of[r.base as usize];
        if b == NONE {
            continue;
        }
        let base = &mut recs[b as usize];
        base.alloc += r.alloc;
        if r.has_size && !base.has_size {
            base.size = r.size;
            base.has_size = true;
        }
        if r.names > 0 {
            more.entry(b).or_default().push((r.names_at, r.names));
        }
    }

    let root_rec = rec_of.get(root as usize).copied().unwrap_or(NONE);
    let root_entry = if root_rec == NONE {
        Entry::default()
    } else {
        let r = recs[root_rec as usize];
        Entry {
            alloc: r.alloc,
            modified: r.modified,
            ..Entry::default()
        }
    };
    let mut b = Builder::new(root_name, root_entry);
    let mut node_of = vec![NONE; total];
    if (root as usize) < total {
        node_of[root as usize] = ROOT;
    }
    let mut pending: Vec<(u32, u64)> = Vec::with_capacity(recs.len());
    let mut own: Vec<Name> = Vec::new();
    for (i, r) in recs.iter().enumerate() {
        if r.base != NONE || r.number == root || r.names == 0 && !more.contains_key(&(i as u32)) {
            continue;
        }
        own.clear();
        own.extend_from_slice(
            &names[r.names_at as usize..(r.names_at + u32::from(r.names)) as usize],
        );
        if let Some(extra) = more.get(&(i as u32)) {
            for &(at, n) in extra {
                own.extend_from_slice(&names[at as usize..(at + u32::from(n)) as usize]);
            }
        }
        // The short 8.3 names only when there is nothing else.
        if own.iter().any(|n| n.namespace != NAMESPACE_DOS) {
            own.retain(|n| n.namespace != NAMESPACE_DOS);
        } else {
            own.truncate(1);
        }
        let mut flags = if r.dir { DIR } else { 0 };
        if r.attrs & 0x2 != 0 {
            flags |= HIDDEN;
        }
        if r.attrs & 0x4 != 0 {
            flags |= SYSTEM;
        }
        if r.attrs & 0x400 != 0 {
            flags |= LINK;
        }
        if r.number < FIRST_USER_RECORD {
            flags |= META;
        }
        for (k, n) in own.iter().enumerate() {
            let name = &text[n.at as usize..(n.at + n.len) as usize];
            // Further names of a file share its size; a folder has one name.
            if k > 0
                && (r.dir
                    || own[..k].iter().any(|o| {
                        o.parent == n.parent
                            && text[o.at as usize..(o.at + o.len) as usize] == *name
                    }))
            {
                continue;
            }
            let entry = Entry {
                size: r.size,
                alloc: r.alloc,
                modified: r.modified,
                flags,
                link: (own.len() > 1).then_some((0, u64::from(r.number))),
            };
            let id = b.push(NONE, name, entry);
            if k == 0 {
                node_of[r.number as usize] = id;
            }
            pending.push((id, n.parent));
        }
    }

    let mut orphans = NONE;
    for (id, parent) in pending {
        let number = (parent & REF_MASK) as usize;
        let seq = (parent >> 48) as u16;
        let ok = number < total
            && node_of[number] != NONE
            && rec_of[number] != NONE
            && recs[rec_of[number] as usize].dir
            && (seq == 0 || recs[rec_of[number] as usize].seq == seq);
        if ok {
            b.set_parent(id, node_of[number]);
        } else if u64::from(root) == ROOT_RECORD {
            if orphans == NONE {
                orphans = b.push(
                    ROOT,
                    "<files without a folder>",
                    Entry {
                        flags: DIR | ORPHANS,
                        ..Entry::default()
                    },
                );
            }
            b.set_parent(id, orphans);
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1024-byte record with the given attributes, written as NTFS
    /// writes it to disk (check values in place).
    fn record(number: u32, flags: u16, base: u64, attrs: &[Vec<u8>]) -> Vec<u8> {
        let mut r = vec![0u8; 1024];
        r[0..4].copy_from_slice(b"FILE");
        r[4..6].copy_from_slice(&0x30u16.to_le_bytes());
        r[6..8].copy_from_slice(&3u16.to_le_bytes());
        r[0x10..0x12].copy_from_slice(&1u16.to_le_bytes());
        r[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
        r[0x16..0x18].copy_from_slice(&flags.to_le_bytes());
        r[0x20..0x28].copy_from_slice(&base.to_le_bytes());
        r[0x2C..0x30].copy_from_slice(&number.to_le_bytes());
        let mut at = 0x38;
        for a in attrs {
            r[at..at + a.len()].copy_from_slice(a);
            at += a.len();
        }
        r[at..at + 4].copy_from_slice(&ATTR_END.to_le_bytes());
        r[0x18..0x1C].copy_from_slice(&((at + 8) as u32).to_le_bytes());
        // The check value goes over the end of each 512-byte stride.
        let check = [0xAB, 0xCD];
        r[0x30..0x32].copy_from_slice(&check);
        for i in 1..3 {
            let end = i * 512;
            let saved = [r[end - 2], r[end - 1]];
            r[0x30 + 2 * i..0x30 + 2 * i + 2].copy_from_slice(&saved);
            r[end - 2..end].copy_from_slice(&check);
        }
        r
    }

    fn resident(kind: u32, value: &[u8]) -> Vec<u8> {
        let len = (0x18 + value.len()).next_multiple_of(8);
        let mut a = vec![0u8; len];
        a[0..4].copy_from_slice(&kind.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[0x10..0x14].copy_from_slice(&(value.len() as u32).to_le_bytes());
        a[0x14..0x16].copy_from_slice(&0x18u16.to_le_bytes());
        a[0x18..0x18 + value.len()].copy_from_slice(value);
        a
    }

    fn file_name(parent: u64, namespace: u8, name: &str) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut v = vec![0u8; 0x42 + units.len() * 2];
        v[0..8].copy_from_slice(&parent.to_le_bytes());
        v[0x40] = units.len() as u8;
        v[0x41] = namespace;
        for (i, u) in units.iter().enumerate() {
            v[0x42 + 2 * i..0x44 + 2 * i].copy_from_slice(&u.to_le_bytes());
        }
        resident(ATTR_FILE_NAME, &v)
    }

    fn data(size: u64, alloc: u64) -> Vec<u8> {
        let mut a = vec![0u8; 0x48];
        a[0..4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        a[4..8].copy_from_slice(&0x48u32.to_le_bytes());
        a[8] = 1;
        a[0x20..0x22].copy_from_slice(&0x40u16.to_le_bytes());
        a[0x28..0x30].copy_from_slice(&alloc.to_le_bytes());
        a[0x30..0x38].copy_from_slice(&size.to_le_bytes());
        a
    }

    fn standard(modified_ft: u64, attrs: u32) -> Vec<u8> {
        let mut v = vec![0u8; 0x48];
        v[8..16].copy_from_slice(&modified_ft.to_le_bytes());
        v[0x20..0x24].copy_from_slice(&attrs.to_le_bytes());
        resident(ATTR_STANDARD_INFORMATION, &v)
    }

    fn reference(number: u64, seq: u16) -> u64 {
        number | (u64::from(seq) << 48)
    }

    #[test]
    fn fixup_restores_the_stride_ends() {
        let mut r = record(30, 1, 0, &[]);
        assert_eq!(&r[510..512], &[0xAB, 0xCD]);
        assert!(fixup(&mut r));
        assert_eq!(&r[510..512], &[0, 0]);
        let mut torn = record(30, 1, 0, &[]);
        torn[1022] = 0;
        assert!(!fixup(&mut torn), "a torn record is refused");
    }

    #[test]
    fn decodes_data_runs() {
        // 0x18 clusters at 0x5634, then 0x10 at -0x10 from there, a hole of 4.
        let b = [0x21, 0x18, 0x34, 0x56, 0x11, 0x10, 0xF0, 0x01, 0x04, 0x00];
        assert_eq!(
            runs(&b).unwrap(),
            [(Some(0x5634), 0x18), (Some(0x5624), 0x10), (None, 4)]
        );
    }

    #[test]
    fn parses_records_into_a_tree() {
        let mut out = Parsed::default();
        let mut parse_one = |mut r: Vec<u8>, number| parse(&mut r, number, 4096, &mut out);
        // The root folder, a folder under it, a file with a long and a
        // short name and a second name elsewhere, a file stored inside its
        // record, and a file whose folder is gone.
        parse_one(record(5, 3, 0, &[file_name(reference(5, 1), 3, ".")]), 5);
        parse_one(
            record(
                30,
                3,
                0,
                &[standard(0, 0), file_name(reference(5, 1), 3, "Games")],
            ),
            30,
        );
        parse_one(
            record(
                31,
                1,
                0,
                &[
                    standard(133_000_000_000_000_000, 0x2),
                    file_name(reference(30, 1), 1, "Very Long Name.pak"),
                    file_name(reference(30, 1), 2, "VERYLO~1.PAK"),
                    file_name(reference(5, 1), 0, "hardlink.pak"),
                    data(5000, 8192),
                ],
            ),
            31,
        );
        parse_one(
            record(
                32,
                1,
                0,
                &[
                    file_name(reference(5, 1), 3, "tiny.txt"),
                    resident(ATTR_DATA, b"hello"),
                ],
            ),
            32,
        );
        parse_one(
            record(
                33,
                1,
                0,
                &[file_name(reference(99, 1), 3, "lost.bin"), data(10, 4096)],
            ),
            33,
        );
        let tree = build(vec![out], 40, ROOT_RECORD as u32, "C:\\");
        let tree = crate::space::Tree::assemble(
            tree.finish(&Progress::default()),
            std::path::Path::new("C:\\"),
            crate::space::Method::Mft,
        );
        let root = tree.nodes[ROOT as usize];
        assert_eq!(
            root.size,
            5000 + 5 + 10,
            "the second name is not counted again"
        );
        assert_eq!(root.alloc, 8192 + 4096);
        let names: Vec<&str> = root.children().map(|i| tree.name(i)).collect();
        assert_eq!(
            names,
            [
                "Games",
                "<files without a folder>",
                "tiny.txt",
                "hardlink.pak"
            ]
        );
        let games = root.first;
        let pak = tree.nodes[games as usize].first;
        assert_eq!(
            tree.name(pak),
            "Very Long Name.pak",
            "the short name is dropped"
        );
        let pak = tree.nodes[pak as usize];
        assert_eq!(pak.size, 5000);
        assert_eq!(pak.flags & HIDDEN, HIDDEN);
        assert!(pak.modified > 1_600_000_000);
        let link = root.children().last().unwrap();
        assert_eq!(
            tree.nodes[link as usize].flags & crate::space::EXTRA_LINK,
            crate::space::EXTRA_LINK
        );
    }

    #[test]
    fn a_folder_scan_keeps_only_what_is_under_it() {
        let mut out = Parsed::default();
        let mut parse_one = |mut r: Vec<u8>, number| parse(&mut r, number, 4096, &mut out);
        parse_one(record(5, 3, 0, &[file_name(reference(5, 1), 3, ".")]), 5);
        parse_one(
            record(30, 3, 0, &[file_name(reference(5, 1), 3, "Games")]),
            30,
        );
        parse_one(
            record(
                31,
                1,
                0,
                &[file_name(reference(30, 1), 3, "in.pak"), data(7, 4096)],
            ),
            31,
        );
        parse_one(
            record(
                32,
                1,
                0,
                &[file_name(reference(5, 1), 3, "out.pak"), data(9, 4096)],
            ),
            32,
        );
        let b = build(vec![out], 40, 30, "C:\\Games");
        let tree = crate::space::Tree::assemble(
            b.finish(&Progress::default()),
            std::path::Path::new("C:\\Games"),
            crate::space::Method::Mft,
        );
        assert_eq!(tree.nodes.len(), 2);
        assert_eq!(tree.nodes[ROOT as usize].size, 7);
    }
}
