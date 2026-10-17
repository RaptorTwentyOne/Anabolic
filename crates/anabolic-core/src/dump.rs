//! Memory dumps of a process, as Task Manager's "Create dump file" writes
//! them, for a debugger. The process is cloned first (a process snapshot,
//! copy-on-write) and the dump written from the clone, so it is frozen for
//! milliseconds rather than for as long as the file takes to write. Opening
//! a process to read all of its memory is as invasive as inspection gets:
//! the UI asks first.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{GENERIC_WRITE, HANDLE, S_FALSE};
use windows::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, GetFileSizeEx,
};
use windows::Win32::System::Diagnostics::Debug::{
    IsProcessSnapshotCallback, MINIDUMP_CALLBACK_INFORMATION, MINIDUMP_CALLBACK_INPUT,
    MINIDUMP_CALLBACK_OUTPUT, MINIDUMP_TYPE, MiniDumpIgnoreInaccessibleMemory,
    MiniDumpWithFullMemory, MiniDumpWithFullMemoryInfo, MiniDumpWithHandleData,
    MiniDumpWithIndirectlyReferencedMemory, MiniDumpWithProcessThreadData, MiniDumpWithThreadInfo,
    MiniDumpWithTokenInformation, MiniDumpWithUnloadedModules, MiniDumpWriteDump,
};
use windows::Win32::System::Diagnostics::ProcessSnapshotting::{
    HPSS, PSS_CAPTURE_FLAGS, PSS_CAPTURE_HANDLE_BASIC_INFORMATION,
    PSS_CAPTURE_HANDLE_NAME_INFORMATION, PSS_CAPTURE_HANDLE_TYPE_SPECIFIC_INFORMATION,
    PSS_CAPTURE_HANDLES, PSS_CAPTURE_THREAD_CONTEXT, PSS_CAPTURE_THREADS, PSS_CAPTURE_VA_CLONE,
    PSS_CREATE_RELEASE_SECTION, PSS_CREATE_USE_VM_ALLOCATIONS, PssCaptureSnapshot, PssFreeSnapshot,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, PROCESS_CREATE_PROCESS, PROCESS_DUP_HANDLE, PROCESS_QUERY_INFORMATION,
    PROCESS_VM_READ,
};
use windows::core::BOOL;

use crate::model::ProcKey;
use crate::util::{OwnedHandle, error_text, open_process, wide};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DumpKind {
    /// All of its memory: what a debugger needs to look at anything. As
    /// big as the process.
    Full,
    /// Threads, stacks, modules and handles: enough for "where is it stuck".
    Small,
}

impl DumpKind {
    fn flags(self) -> MINIDUMP_TYPE {
        let common = MiniDumpWithHandleData.0
            | MiniDumpWithThreadInfo.0
            | MiniDumpWithUnloadedModules.0
            | MiniDumpWithProcessThreadData.0
            | MiniDumpWithTokenInformation.0
            | MiniDumpIgnoreInaccessibleMemory.0;
        MINIDUMP_TYPE(match self {
            DumpKind::Full => common | MiniDumpWithFullMemory.0 | MiniDumpWithFullMemoryInfo.0,
            DumpKind::Small => common | MiniDumpWithIndirectlyReferencedMemory.0,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            DumpKind::Full => "full",
            DumpKind::Small => "small",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DumpReport {
    pub path: PathBuf,
    pub bytes: u64,
    pub took: Duration,
    /// Written from a clone; false when Windows would not clone the
    /// process and it was frozen for the whole write.
    pub cloned: bool,
}

/// Tells MiniDumpWriteDump that the handle it was given is a snapshot.
unsafe extern "system" fn snapshot_callback(
    _param: *mut core::ffi::c_void,
    input: *const MINIDUMP_CALLBACK_INPUT,
    output: *mut MINIDUMP_CALLBACK_OUTPUT,
) -> BOOL {
    unsafe {
        if (*input).CallbackType == IsProcessSnapshotCallback.0 as u32 {
            (*output).Anonymous.Status = S_FALSE;
        }
    }
    true.into()
}

/// A name for the file: `name-pid-YYYYMMDD-HHMMSS.dmp`.
pub fn file_name(name: &str, pid: u32, kind: DumpKind) -> String {
    let stem = name.trim_end_matches(".exe").trim_end_matches(".EXE");
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
    format!("{stem}-{pid}-{stamp}{small}.dmp")
}

/// The processes that hold the machine's sign-in secrets: the Local
/// Security Authority and its isolated half. Their memory is never written
/// to a file, whoever asks.
const REFUSED: &[&str] = &["lsass.exe", "lsaiso.exe"];

/// Whether a process of this name is never dumped.
pub fn refused(name: &str) -> bool {
    REFUSED.iter().any(|r| name.eq_ignore_ascii_case(r))
}

/// Writes a dump of `key` into `dir`, which is created if needed.
pub fn write(key: ProcKey, name: &str, kind: DumpKind, dir: &Path) -> Result<DumpReport, String> {
    if refused(name) {
        return Err(format!(
            "{name} holds the sign-in secrets of this PC: Anabolic does not write its memory to a file."
        ));
    }
    let started = Instant::now();
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let path = dir.join(file_name(name, key.pid, kind));
    let process = open_process(
        key,
        PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_DUP_HANDLE | PROCESS_CREATE_PROCESS,
    )
    .or_else(|_| open_process(key, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ))
    .map_err(|e| format!("Could not open {name} ({}): {}", key.pid, error_text(&e)))?;
    let path_w = wide(&path.to_string_lossy());
    let file = unsafe {
        CreateFileW(
            windows::core::PCWSTR(path_w.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ,
            None,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map(OwnedHandle)
    .map_err(|e| format!("Could not create {}: {}", path.display(), error_text(&e)))?;

    // The clone: memory copy-on-write, threads with their registers,
    // handles with names.
    let flags = PSS_CAPTURE_FLAGS(
        PSS_CAPTURE_VA_CLONE.0
            | PSS_CAPTURE_HANDLES.0
            | PSS_CAPTURE_HANDLE_NAME_INFORMATION.0
            | PSS_CAPTURE_HANDLE_BASIC_INFORMATION.0
            | PSS_CAPTURE_HANDLE_TYPE_SPECIFIC_INFORMATION.0
            | PSS_CAPTURE_THREADS.0
            | PSS_CAPTURE_THREAD_CONTEXT.0
            | PSS_CREATE_USE_VM_ALLOCATIONS.0
            | PSS_CREATE_RELEASE_SECTION.0,
    );
    // CONTEXT_ALL for x64.
    const CONTEXT_ALL: u32 = 0x0010_001F;
    let mut snapshot = HPSS::default();
    let captured =
        unsafe { PssCaptureSnapshot(process.raw(), flags, Some(CONTEXT_ALL), &mut snapshot) } == 0;
    let result = if captured {
        let callback = MINIDUMP_CALLBACK_INFORMATION {
            CallbackRoutine: Some(snapshot_callback),
            CallbackParam: std::ptr::null_mut(),
        };
        let written = unsafe {
            MiniDumpWriteDump(
                HANDLE(snapshot.0),
                key.pid,
                file.raw(),
                kind.flags(),
                None,
                None,
                Some(&callback),
            )
        };
        unsafe {
            PssFreeSnapshot(GetCurrentProcess(), snapshot);
        }
        written
    } else {
        unsafe {
            MiniDumpWriteDump(
                process.raw(),
                key.pid,
                file.raw(),
                kind.flags(),
                None,
                None,
                None,
            )
        }
    };
    if let Err(e) = result {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(format!(
            "Could not write a dump of {name} ({}): {}",
            key.pid,
            error_text(&e)
        ));
    }
    let mut bytes = 0i64;
    unsafe {
        let _ = GetFileSizeEx(file.raw(), &mut bytes);
    }
    Ok(DumpReport {
        path,
        bytes: bytes.max(0) as u64,
        took: started.elapsed(),
        cloned: captured,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dumps this test process (nothing else is touched).
    #[test]
    fn dumps_itself_from_a_clone() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("anabolic-dump-test-{pid}"));
        let key = ProcKey {
            pid,
            create_time: 0,
        };
        let report = write(key, "self.exe", DumpKind::Small, &dir).expect("dump written");
        assert!(report.cloned);
        assert!(report.bytes > 4096, "{} bytes", report.bytes);
        let head = std::fs::read(&report.path).unwrap();
        assert_eq!(&head[..4], b"MDMP");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Refused by name before anything is opened or created.
    #[test]
    fn never_dumps_the_sign_in_secrets() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("anabolic-dump-refused-{pid}"));
        let key = ProcKey {
            pid,
            create_time: 0,
        };
        assert!(write(key, "LSASS.EXE", DumpKind::Small, &dir).is_err());
        assert!(!dir.exists());
        assert!(refused("LsaIso.exe"));
        assert!(!refused("lsass2.exe"));
    }
}
