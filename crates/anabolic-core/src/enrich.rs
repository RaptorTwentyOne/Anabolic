//! Slow, per-process facts (path, command line, owner, icon, version info) are
//! gathered here, off both the UI and the collector thread. Results stream to
//! the UI over a channel; nothing ever waits for them.

use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Sender, unbounded};

use crate::model::ProcKey;

#[cfg_attr(windows, path = "windows/enrich.rs")]
#[cfg_attr(target_os = "linux", path = "linux/enrich.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    X64,
    X86,
    Arm64,
    Other(u16),
}

impl Arch {
    pub fn label(self) -> &'static str {
        match self {
            Arch::X64 => "x64",
            Arch::X86 => "x86",
            Arch::Arm64 => "ARM64",
            Arch::Other(_) => "?",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Integrity {
    Untrusted,
    Low,
    Medium,
    MediumPlus,
    High,
    System,
    Protected,
}

impl Integrity {
    pub fn label(self) -> &'static str {
        match self {
            Integrity::Untrusted => "Untrusted",
            Integrity::Low => "Low",
            Integrity::Medium => "Medium",
            Integrity::MediumPlus => "Medium+",
            Integrity::High => "High",
            Integrity::System => "System",
            Integrity::Protected => "Protected",
        }
    }
}

/// A decoded application icon, RGBA8, shared between processes of one image.
#[derive(Debug)]
pub struct Icon {
    /// Stable id the UI can key its texture cache with.
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct ProcessDetails {
    pub path: Option<String>,
    pub command_line: Option<String>,
    pub description: Option<String>,
    pub company: Option<String>,
    pub version: Option<String>,
    pub user: Option<String>,
    pub arch: Option<Arch>,
    pub elevated: Option<bool>,
    pub integrity: Option<Integrity>,
    /// Marked critical: terminating it bugchecks the machine.
    pub critical: bool,
    pub icon: Option<Arc<Icon>>,
    /// Why the process could not be opened, if it could not.
    pub open_error: Option<String>,
}

pub struct EnrichRequest {
    pub key: ProcKey,
    pub sid: Option<Arc<[u8]>>,
    /// Image path as reported by the process scan, when it had one.
    pub nt_path: Option<String>,
}

pub struct Enriched {
    pub key: ProcKey,
    pub details: Arc<ProcessDetails>,
}

pub(crate) fn spawn(results: Sender<Enriched>) -> Sender<EnrichRequest> {
    let (tx, rx) = unbounded::<EnrichRequest>();
    thread::Builder::new()
        .name("anabolic-enrich".into())
        .spawn(move || imp::worker(rx, results))
        .expect("failed to spawn the enrichment thread");
    tx
}
