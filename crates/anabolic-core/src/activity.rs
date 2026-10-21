//! What a program does, step by step, the way Process Monitor shows it:
//! the processes it starts, the code it loads, every file it opens,
//! creates, reads, writes, renames or deletes, every registry key and value
//! it touches (Windows), its network connections, with the result of each,
//! and, when asked, every system call it makes (Linux).
//!
//! All of it comes from the kernel's own reporting: no driver, no hook, and
//! the program is never opened: recording a game touches it no more than
//! watching it in the process list.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

#[cfg_attr(windows, path = "windows/activity.rs")]
#[cfg_attr(target_os = "linux", path = "linux/activity.rs")]
mod imp;
pub use imp::*;

/// Events kept for the UI before new ones are only counted.
pub const MAX_EVENTS: u64 = 1_000_000;

/// A path as a number, to tell paths apart without keeping them.
fn path_hash(path: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Group {
    Process,
    File,
    Registry,
    Network,
    #[cfg(target_os = "linux")]
    Syscall,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ActivityKind {
    ProcessStart,
    ProcessExit,
    ImageLoad,
    FileOpen,
    FileCreate,
    FileRead,
    FileWrite,
    FileDelete,
    FileRename,
    FileSetInfo,
    FileListDir,
    RegOpenKey,
    RegCreateKey,
    RegQueryValue,
    RegSetValue,
    RegDeleteValue,
    RegDeleteKey,
    RegEnumKey,
    RegEnumValue,
    NetConnect,
    NetAccept,
    NetDisconnect,
    NetSend,
    NetReceive,
    /// A system call, by its number.
    #[cfg(target_os = "linux")]
    Syscall(u16),
}

impl ActivityKind {
    pub fn label(self) -> &'static str {
        match self {
            ActivityKind::ProcessStart => "Start process",
            ActivityKind::ProcessExit => "Exit",
            ActivityKind::ImageLoad => "Load image",
            ActivityKind::FileOpen => "Open file",
            ActivityKind::FileCreate => "Create file",
            ActivityKind::FileRead => "Read file",
            ActivityKind::FileWrite => "Write file",
            ActivityKind::FileDelete => "Delete file",
            ActivityKind::FileRename => "Rename file",
            ActivityKind::FileSetInfo => "Set file info",
            ActivityKind::FileListDir => "List folder",
            ActivityKind::RegOpenKey => "Open key",
            ActivityKind::RegCreateKey => "Create key",
            ActivityKind::RegQueryValue => "Read value",
            ActivityKind::RegSetValue => "Set value",
            ActivityKind::RegDeleteValue => "Delete value",
            ActivityKind::RegDeleteKey => "Delete key",
            ActivityKind::RegEnumKey => "List subkeys",
            ActivityKind::RegEnumValue => "List values",
            ActivityKind::NetConnect => "Connect",
            ActivityKind::NetAccept => "Accept",
            ActivityKind::NetDisconnect => "Disconnect",
            ActivityKind::NetSend => "Send",
            ActivityKind::NetReceive => "Receive",
            #[cfg(target_os = "linux")]
            ActivityKind::Syscall(nr) => {
                crate::sysinfo::syscall_name(u32::from(nr)).unwrap_or("system call")
            }
        }
    }

    pub fn group(self) -> Group {
        use ActivityKind::*;
        match self {
            ProcessStart | ProcessExit | ImageLoad => Group::Process,
            FileOpen | FileCreate | FileRead | FileWrite | FileDelete | FileRename
            | FileSetInfo | FileListDir => Group::File,
            RegOpenKey | RegCreateKey | RegQueryValue | RegSetValue | RegDeleteValue
            | RegDeleteKey | RegEnumKey | RegEnumValue => Group::Registry,
            NetConnect | NetAccept | NetDisconnect | NetSend | NetReceive => Group::Network,
            #[cfg(target_os = "linux")]
            Syscall(_) => Group::Syscall,
        }
    }

    /// Changes something (as opposed to looking).
    pub fn changes(self) -> bool {
        use ActivityKind::*;
        matches!(
            self,
            FileCreate
                | FileWrite
                | FileDelete
                | FileRename
                | FileSetInfo
                | RegCreateKey
                | RegSetValue
                | RegDeleteValue
                | RegDeleteKey
        )
    }
}

/// One step of the timeline.
#[derive(Clone, Debug)]
pub struct ActivityEvent {
    /// Order of events; also unique.
    pub seq: u64,
    pub at: SystemTime,
    pub pid: u32,
    pub tid: u32,
    pub kind: ActivityKind,
    /// The file (drive-letter form), key (HKLM\...), endpoint (1.2.3.4:443)
    /// or child program.
    pub path: Arc<str>,
    /// Value name, offset and size, command line, options: what else there
    /// is to say.
    pub detail: String,
    /// NTSTATUS (files, registry), exit code (process exit), errno (Linux);
    /// None when the event carries no result.
    pub status: Option<u32>,
    /// Merged events: how many operations this one stands for.
    pub count: u32,
    /// Bytes read, written, sent or received.
    pub bytes: u64,
    /// Time spent in it (system calls; all of them when merged); zero when
    /// not measured.
    pub took: Duration,
}

/// One system call's totals over a recording, as `strace -c` gives them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SyscallStat {
    pub name: &'static str,
    pub calls: u64,
    /// Calls that returned an error.
    pub errors: u64,
    /// Time inside the calls that returned.
    pub time: Duration,
}

/// What the recording found so far.
#[derive(Clone, Debug, Default)]
pub struct ActivitySummary {
    /// Every event the kernel delivered, recorded processes or not.
    pub seen: u64,
    /// Events handed over (merged ones count once).
    pub kept: u64,
    /// Past `MAX_EVENTS`: counted, not kept.
    pub dropped: u64,
    /// Events the kernel itself dropped (the trace fell behind).
    pub lost: u32,
    pub processes_started: u32,
    pub images_loaded: u32,
    pub files_created: u32,
    /// Distinct files written to.
    pub files_written: u32,
    pub files_deleted: u32,
    pub values_set: u32,
    pub keys_created: u32,
    pub keys_deleted: u32,
    /// Distinct remote endpoints.
    pub endpoints: u32,
    pub sent: u64,
    pub received: u64,
    /// System calls by call, the most time first (Linux, when asked).
    pub syscalls: Vec<SyscallStat>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RecorderState {
    /// Learning key and file names, or waiting for a launched program.
    Starting,
    Recording,
    Failed(String),
}

/// How a program is started for recording.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LaunchHow {
    /// As the signed-in user, through Explorer, when Anabolic is elevated
    /// (a program should not inherit administrator rights from it); else
    /// directly. `show` is the ShowWindow command.
    Normal { show: i32 },
    /// CreateProcess from Anabolic, no window: for tests.
    Hidden,
}
