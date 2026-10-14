//! Hand-written bindings for the native API pieces Win32 does not expose.
//!
//! Layouts follow phnt (the System Informer headers) and are x64 only; every
//! struct we read straight out of a kernel buffer has its size pinned below.

#![allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code
)]

use core::ffi::c_void;
use core::mem::size_of;

use windows::Win32::Foundation::HANDLE;

pub type NTSTATUS = i32;

pub const STATUS_SUCCESS: NTSTATUS = 0;
pub const STATUS_BUFFER_OVERFLOW: NTSTATUS = 0x8000_0005_u32 as i32;
pub const STATUS_INFO_LENGTH_MISMATCH: NTSTATUS = 0xC000_0004_u32 as i32;
pub const STATUS_BUFFER_TOO_SMALL: NTSTATUS = 0xC000_0023_u32 as i32;

#[inline]
pub fn nt_success(status: NTSTATUS) -> bool {
    status >= 0
}

// SYSTEM_INFORMATION_CLASS
pub const SystemPerformanceInformation: u32 = 2;
pub const SystemProcessInformation: u32 = 5;
pub const SystemPageFileInformation: u32 = 18;
pub const SystemProcessorPerformanceInformation: u32 = 8;
pub const SystemModuleInformation: u32 = 11;
pub const SystemInterruptInformation: u32 = 23;
pub const SystemExtendedProcessInformation: u32 = 57;
pub const SystemExtendedHandleInformation: u32 = 64;
pub const SystemMemoryListInformation: u32 = 80;
pub const SystemProcessIdInformation: u32 = 88;
pub const SystemProcessorPerformanceDistribution: u32 = 100;
pub const SystemFullProcessInformation: u32 = 148;

// PROCESSINFOCLASS
pub const ProcessBreakOnTermination: u32 = 29;
pub const ProcessIoPriority: u32 = 33;
pub const ProcessCommandLineInformation: u32 = 60;

// OBJECT_INFORMATION_CLASS
pub const ObjectNameInformation: u32 = 1;
pub const ObjectTypesInformation: u32 = 3;

// IO_PRIORITY_HINT
pub const IoPriorityNormal: u32 = 2;
pub const IoPriorityHigh: u32 = 3;

// KTHREAD_STATE / KWAIT_REASON values we care about.
pub const ThreadStateWaiting: u32 = 5;
pub const WaitReasonSuspended: u32 = 5;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UNICODE_STRING {
    pub Length: u16,
    pub MaximumLength: u16,
    pub Buffer: *mut u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CLIENT_ID {
    pub UniqueProcess: usize,
    pub UniqueThread: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SYSTEM_THREAD_INFORMATION {
    pub KernelTime: i64,
    pub UserTime: i64,
    pub CreateTime: i64,
    pub WaitTime: u32,
    pub StartAddress: usize,
    pub ClientId: CLIENT_ID,
    pub Priority: i32,
    pub BasePriority: i32,
    pub ContextSwitches: u32,
    pub ThreadState: u32,
    pub WaitReason: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SYSTEM_EXTENDED_THREAD_INFORMATION {
    pub ThreadInfo: SYSTEM_THREAD_INFORMATION,
    pub StackBase: usize,
    pub StackLimit: usize,
    pub Win32StartAddress: usize,
    pub TebBase: usize,
    pub Reserved2: usize,
    pub Reserved3: usize,
    pub Reserved4: usize,
}

/// Fixed part of a process entry; the thread array follows it directly.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SYSTEM_PROCESS_INFORMATION {
    pub NextEntryOffset: u32,
    pub NumberOfThreads: u32,
    pub WorkingSetPrivateSize: i64,
    pub HardFaultCount: u32,
    pub NumberOfThreadsHighWatermark: u32,
    pub CycleTime: u64,
    pub CreateTime: i64,
    pub UserTime: i64,
    pub KernelTime: i64,
    pub ImageName: UNICODE_STRING,
    pub BasePriority: i32,
    pub UniqueProcessId: usize,
    pub InheritedFromUniqueProcessId: usize,
    pub HandleCount: u32,
    pub SessionId: u32,
    pub UniqueProcessKey: usize,
    pub PeakVirtualSize: usize,
    pub VirtualSize: usize,
    pub PageFaultCount: u32,
    pub PeakWorkingSetSize: usize,
    pub WorkingSetSize: usize,
    pub QuotaPeakPagedPoolUsage: usize,
    pub QuotaPagedPoolUsage: usize,
    pub QuotaPeakNonPagedPoolUsage: usize,
    pub QuotaNonPagedPoolUsage: usize,
    pub PagefileUsage: usize,
    pub PeakPagefileUsage: usize,
    pub PrivatePageCount: usize,
    pub ReadOperationCount: i64,
    pub WriteOperationCount: i64,
    pub OtherOperationCount: i64,
    pub ReadTransferCount: i64,
    pub WriteTransferCount: i64,
    pub OtherTransferCount: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PROCESS_DISK_COUNTERS {
    pub BytesRead: u64,
    pub BytesWritten: u64,
    pub ReadOperationCount: u64,
    pub WriteOperationCount: u64,
    pub FlushOperationCount: u64,
}

/// Leading, version-stable part of SYSTEM_PROCESS_INFORMATION_EXTENSION, which
/// SystemFullProcessInformation appends after each process' thread array.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SYSTEM_PROCESS_INFORMATION_EXTENSION {
    pub DiskCounters: PROCESS_DISK_COUNTERS,
    pub ContextSwitches: u64,
    pub Flags: u32,
    /// Offset of the user SID, relative to the start of this extension.
    pub UserSidOffset: u32,
    pub PackageFullNameOffset: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION {
    pub IdleTime: i64,
    /// Includes idle time.
    pub KernelTime: i64,
    pub UserTime: i64,
    pub DpcTime: i64,
    pub InterruptTime: i64,
    pub InterruptCount: u32,
}

/// Leading part of SYSTEM_PERFORMANCE_INFORMATION (the full struct keeps
/// growing between Windows releases; we only read this stable prefix).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SYSTEM_PERFORMANCE_INFORMATION_PREFIX {
    pub IdleProcessTime: i64,
    pub IoReadTransferCount: i64,
    pub IoWriteTransferCount: i64,
    pub IoOtherTransferCount: i64,
    pub IoReadOperationCount: u32,
    pub IoWriteOperationCount: u32,
    pub IoOtherOperationCount: u32,
    pub AvailablePages: u32,
    pub CommittedPages: u32,
    pub CommitLimit: u32,
    pub PeakCommitment: u32,
    pub PageFaultCount: u32,
    pub CopyOnWriteCount: u32,
    pub TransitionCount: u32,
    pub CacheTransitionCount: u32,
    pub DemandZeroCount: u32,
    pub PageReadCount: u32,
    pub PageReadIoCount: u32,
    pub CacheReadCount: u32,
    pub CacheIoCount: u32,
    pub DirtyPagesWriteCount: u32,
    pub DirtyWriteIoCount: u32,
    pub MappedPagesWriteCount: u32,
    pub MappedWriteIoCount: u32,
    pub PagedPoolPages: u32,
    pub NonPagedPoolPages: u32,
    pub PagedPoolAllocs: u32,
    pub PagedPoolFrees: u32,
    pub NonPagedPoolAllocs: u32,
    pub NonPagedPoolFrees: u32,
    pub FreeSystemPtes: u32,
    pub ResidentSystemCodePage: u32,
    pub TotalSystemDriverPages: u32,
    pub TotalSystemCodePages: u32,
    pub NonPagedPoolLookasideHits: u32,
    pub PagedPoolLookasideHits: u32,
    pub AvailablePagedPoolPages: u32,
    pub ResidentSystemCachePage: u32,
    pub ResidentPagedPoolPage: u32,
    pub ResidentSystemDriverPage: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SYSTEM_PAGEFILE_INFORMATION {
    pub NextEntryOffset: u32,
    /// Pages.
    pub TotalSize: u32,
    pub TotalInUse: u32,
    pub PeakUsage: u32,
    pub PageFileName: UNICODE_STRING,
}

/// Per processor; despite the name it also carries context switches.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SYSTEM_INTERRUPT_INFORMATION {
    pub ContextSwitches: u32,
    pub DpcCount: u32,
    pub DpcRate: u32,
    pub TimeIncrement: u32,
    pub DpcBypassCount: u32,
    pub ApcBypassCount: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SYSTEM_MEMORY_LIST_INFORMATION {
    pub ZeroPageCount: usize,
    pub FreePageCount: usize,
    pub ModifiedPageCount: usize,
    pub ModifiedNoWritePageCount: usize,
    pub BadPageCount: usize,
    /// Standby list, split by page priority.
    pub PageCountByPriority: [usize; 8],
    pub RepurposedPagesByPriority: [usize; 8],
    pub ModifiedPageCountPageFile: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX {
    pub Object: usize,
    pub UniqueProcessId: usize,
    pub HandleValue: usize,
    pub GrantedAccess: u32,
    pub CreatorBackTraceIndex: u16,
    pub ObjectTypeIndex: u16,
    pub HandleAttributes: u32,
    pub Reserved: u32,
}

/// Header of SYSTEM_HANDLE_INFORMATION_EX; the entries follow it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SYSTEM_HANDLE_INFORMATION_EX {
    pub NumberOfHandles: usize,
    pub Reserved: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GENERIC_MAPPING {
    pub GenericRead: u32,
    pub GenericWrite: u32,
    pub GenericExecute: u32,
    pub GenericAll: u32,
}

/// One entry of OBJECT_TYPES_INFORMATION; its name buffer follows it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OBJECT_TYPE_INFORMATION {
    pub TypeName: UNICODE_STRING,
    pub TotalNumberOfObjects: u32,
    pub TotalNumberOfHandles: u32,
    pub TotalPagedPoolUsage: u32,
    pub TotalNonPagedPoolUsage: u32,
    pub TotalNamePoolUsage: u32,
    pub TotalHandleTableUsage: u32,
    pub HighWaterNumberOfObjects: u32,
    pub HighWaterNumberOfHandles: u32,
    pub HighWaterPagedPoolUsage: u32,
    pub HighWaterNonPagedPoolUsage: u32,
    pub HighWaterNamePoolUsage: u32,
    pub HighWaterHandleTableUsage: u32,
    pub InvalidAttributes: u32,
    pub GenericMapping: GENERIC_MAPPING,
    pub ValidAccessMask: u32,
    pub SecurityRequired: u8,
    pub MaintainHandleCount: u8,
    /// Since Windows 8.1.
    pub TypeIndex: u8,
    pub ReservedByte: u8,
    pub PoolType: u32,
    pub DefaultPagedPoolCharge: u32,
    pub DefaultNonPagedPoolCharge: u32,
}

#[repr(C)]
pub struct SYSTEM_PROCESS_ID_INFORMATION {
    pub ProcessId: usize,
    pub ImageName: UNICODE_STRING,
}

const _: () = assert!(size_of::<SYSTEM_THREAD_INFORMATION>() == 0x50);
const _: () = assert!(size_of::<SYSTEM_EXTENDED_THREAD_INFORMATION>() == 0x88);
const _: () = assert!(size_of::<SYSTEM_PROCESS_INFORMATION>() == 0x100);
const _: () = assert!(size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>() == 0x30);
const _: () = assert!(size_of::<SYSTEM_MEMORY_LIST_INFORMATION>() == 0xB0);
const _: () = assert!(size_of::<SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX>() == 0x28);
const _: () = assert!(size_of::<OBJECT_TYPE_INFORMATION>() == 0x68);

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    pub fn NtQuerySystemInformation(
        class: u32,
        info: *mut c_void,
        length: u32,
        return_length: *mut u32,
    ) -> NTSTATUS;

    pub fn NtQuerySystemInformationEx(
        class: u32,
        input: *const c_void,
        input_length: u32,
        info: *mut c_void,
        length: u32,
        return_length: *mut u32,
    ) -> NTSTATUS;

    pub fn NtQueryInformationProcess(
        process: HANDLE,
        class: u32,
        info: *mut c_void,
        length: u32,
        return_length: *mut u32,
    ) -> NTSTATUS;

    pub fn NtSetInformationProcess(
        process: HANDLE,
        class: u32,
        info: *const c_void,
        length: u32,
    ) -> NTSTATUS;

    pub fn NtQueryObject(
        handle: HANDLE,
        class: u32,
        info: *mut c_void,
        length: u32,
        return_length: *mut u32,
    ) -> NTSTATUS;

    pub fn NtSuspendProcess(process: HANDLE) -> NTSTATUS;
    pub fn NtResumeProcess(process: HANDLE) -> NTSTATUS;
    pub fn RtlNtStatusToDosError(status: NTSTATUS) -> u32;
}

/// Human readable text for an NTSTATUS, via its Win32 equivalent.
pub fn status_message(status: NTSTATUS) -> String {
    let win32 = unsafe { RtlNtStatusToDosError(status) };
    let msg =
        windows::core::Error::from_hresult(windows::core::HRESULT::from_win32(win32)).message();
    if msg.is_empty() {
        format!("NTSTATUS 0x{:08X}", status as u32)
    } else {
        format!("{msg} (0x{:08X})", status as u32)
    }
}
