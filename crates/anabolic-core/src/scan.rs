//! Process enumeration: a single NtQuerySystemInformation call returns every
//! process together with its threads, counters and (when elevated) disk I/O
//! and owner SID. No handles are opened, so nothing here can block on a
//! misbehaving process.

use core::mem::size_of;

use crate::model::InfoLevel;
use crate::ntapi::*;

const MAX_BUFFER: usize = 512 * 1024 * 1024;
const SLACK: usize = 128 * 1024;

pub struct ProcessScanner {
    /// u64 storage keeps the kernel's 8-byte aligned structs aligned.
    buf: Vec<u64>,
    len: usize,
    level: InfoLevel,
}

impl Default for ProcessScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessScanner {
    pub fn new() -> Self {
        Self {
            buf: vec![0; 1024 * 1024 / 8],
            len: 0,
            level: InfoLevel::Full,
        }
    }

    pub fn level(&self) -> InfoLevel {
        self.level
    }

    /// Takes a fresh snapshot into the internal buffer, falling back to less
    /// detailed information classes when the richer ones are refused.
    pub fn refresh(&mut self) -> Result<(), NTSTATUS> {
        loop {
            let class = match self.level {
                InfoLevel::Full => SystemFullProcessInformation,
                InfoLevel::Extended => SystemExtendedProcessInformation,
                InfoLevel::Basic => SystemProcessInformation,
            };
            let bytes = self.buf.len() * 8;
            let mut needed = 0u32;
            let status = unsafe {
                NtQuerySystemInformation(
                    class,
                    self.buf.as_mut_ptr().cast(),
                    bytes as u32,
                    &mut needed,
                )
            };
            if nt_success(status) {
                let needed = needed as usize;
                self.len = if needed > 0 && needed <= bytes {
                    needed
                } else {
                    bytes
                };
                return Ok(());
            }
            match status {
                STATUS_INFO_LENGTH_MISMATCH | STATUS_BUFFER_TOO_SMALL | STATUS_BUFFER_OVERFLOW => {
                    let want = (needed as usize).max(bytes * 2) + SLACK;
                    if want > MAX_BUFFER {
                        return Err(status);
                    }
                    self.buf.resize(want.div_ceil(8), 0);
                }
                _ => {
                    self.level = match self.level {
                        InfoLevel::Full => InfoLevel::Extended,
                        InfoLevel::Extended => InfoLevel::Basic,
                        InfoLevel::Basic => return Err(status),
                    };
                }
            }
        }
    }

    pub fn entries(&self) -> Entries<'_> {
        let bytes =
            unsafe { core::slice::from_raw_parts(self.buf.as_ptr().cast::<u8>(), self.len) };
        Entries {
            buf: bytes,
            next: (self.len >= size_of::<SYSTEM_PROCESS_INFORMATION>()).then_some(0),
            level: self.level,
        }
    }
}

pub struct Entries<'a> {
    buf: &'a [u8],
    next: Option<usize>,
    level: InfoLevel,
}

impl<'a> Iterator for Entries<'a> {
    type Item = Entry<'a>;

    fn next(&mut self) -> Option<Entry<'a>> {
        let start = self.next.take()?;
        let spi: SYSTEM_PROCESS_INFORMATION = read(self.buf, start)?;
        let end = match spi.NextEntryOffset as usize {
            0 => self.buf.len(),
            n => {
                self.next = Some(start + n);
                (start + n).min(self.buf.len())
            }
        };
        Some(Entry {
            spi,
            buf: self.buf,
            start,
            end,
            level: self.level,
        })
    }
}

/// Entries are not necessarily 8-byte aligned (the full information class packs
/// variable-length SIDs and package names between them), so every struct is
/// copied out with an unaligned read instead of being referenced in place.
pub struct Entry<'a> {
    pub spi: SYSTEM_PROCESS_INFORMATION,
    buf: &'a [u8],
    start: usize,
    end: usize,
    level: InfoLevel,
}

pub struct ThreadView {
    pub info: SYSTEM_THREAD_INFORMATION,
    pub ext: Option<SYSTEM_EXTENDED_THREAD_INFORMATION>,
}

/// Copies a `T` out of `buf` at `offset`, if it fits.
fn read<T: Copy>(buf: &[u8], offset: usize) -> Option<T> {
    let end = offset.checked_add(size_of::<T>())?;
    if end > buf.len() {
        return None;
    }
    // SAFETY: in bounds; read_unaligned has no alignment requirement and T is plain old data.
    Some(unsafe { buf.as_ptr().add(offset).cast::<T>().read_unaligned() })
}

impl<'a> Entry<'a> {
    pub fn pid(&self) -> u32 {
        self.spi.UniqueProcessId as u32
    }

    /// The image name as UTF-16; empty for the idle process. With
    /// `InfoLevel::Full` this is the full NT path of the executable.
    pub fn image_name(&self) -> Vec<u16> {
        let name = self.spi.ImageName;
        let len = name.Length as usize & !1;
        let base = self.buf.as_ptr() as usize;
        let ptr = name.Buffer as usize;
        if name.Buffer.is_null() || len == 0 || ptr < base || ptr + len > base + self.buf.len() {
            return Vec::new();
        }
        let at = ptr - base;
        self.buf[at..at + len]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect()
    }

    fn thread_stride(&self) -> usize {
        match self.level {
            InfoLevel::Basic => size_of::<SYSTEM_THREAD_INFORMATION>(),
            _ => size_of::<SYSTEM_EXTENDED_THREAD_INFORMATION>(),
        }
    }

    fn threads_end(&self) -> Option<usize> {
        let first = self.start + size_of::<SYSTEM_PROCESS_INFORMATION>();
        let end = first + self.spi.NumberOfThreads as usize * self.thread_stride();
        (end <= self.end).then_some(end)
    }

    pub fn threads(&self) -> impl Iterator<Item = ThreadView> + 'a {
        let count = if self.threads_end().is_some() {
            self.spi.NumberOfThreads as usize
        } else {
            0
        };
        let first = self.start + size_of::<SYSTEM_PROCESS_INFORMATION>();
        let stride = self.thread_stride();
        let extended = self.level != InfoLevel::Basic;
        let buf = self.buf;
        (0..count).filter_map(move |i| {
            let at = first + i * stride;
            if extended {
                let ext: SYSTEM_EXTENDED_THREAD_INFORMATION = read(buf, at)?;
                Some(ThreadView {
                    info: ext.ThreadInfo,
                    ext: Some(ext),
                })
            } else {
                Some(ThreadView {
                    info: read(buf, at)?,
                    ext: None,
                })
            }
        })
    }

    pub fn extension(&self) -> Option<SYSTEM_PROCESS_INFORMATION_EXTENSION> {
        if self.level != InfoLevel::Full {
            return None;
        }
        let at = self.threads_end()?;
        if at + size_of::<SYSTEM_PROCESS_INFORMATION_EXTENSION>() > self.end {
            return None;
        }
        read(self.buf, at)
    }

    /// Owner SID in binary form (needs `InfoLevel::Full`).
    pub fn user_sid(&self) -> Option<&'a [u8]> {
        let offset = self.extension()?.UserSidOffset as usize;
        if offset == 0 {
            return None;
        }
        // Relative to the extension, not to the process entry.
        let at = self.threads_end()? + offset;
        let header = self.buf.get(at..at + 8)?;
        let (revision, sub_authorities) = (header[0], header[1] as usize);
        if revision != 1 || sub_authorities > 15 {
            return None;
        }
        self.buf.get(at..at + 8 + 4 * sub_authorities)
    }

    /// True when every thread sits in a suspended wait.
    pub fn is_suspended(&self) -> bool {
        let mut any = false;
        for t in self.threads() {
            any = true;
            if t.info.ThreadState != ThreadStateWaiting || t.info.WaitReason != WaitReasonSuspended
            {
                return false;
            }
        }
        any
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerates_self_and_system() {
        let mut scanner = ProcessScanner::new();
        scanner.refresh().expect("NtQuerySystemInformation failed");
        let me = std::process::id();
        let mut found_self = false;
        let mut found_system = false;
        let mut count = 0;
        for e in scanner.entries() {
            count += 1;
            if e.pid() == me {
                found_self = true;
                assert!(e.spi.NumberOfThreads >= 1);
                assert_eq!(e.threads().count(), e.spi.NumberOfThreads as usize);
                let name = String::from_utf16_lossy(&e.image_name());
                assert!(
                    name.to_lowercase().contains("anabolic_core"),
                    "unexpected own name {name}"
                );
                if scanner.level() == InfoLevel::Full {
                    assert!(
                        e.user_sid().is_some(),
                        "full info should carry the owner SID"
                    );
                }
            }
            if e.pid() == 4 {
                found_system = true;
                assert_eq!(String::from_utf16_lossy(&e.image_name()), "System");
            }
        }
        assert!(count > 10, "only {count} processes");
        assert!(found_self && found_system);
    }
}
