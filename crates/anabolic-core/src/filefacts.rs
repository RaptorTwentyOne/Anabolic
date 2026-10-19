//! What a program file is: who vouches for it, its SHA-256, how it was
//! built. Reads the file only; nothing runs.

use std::sync::Arc;
use std::time::Duration;

#[cfg_attr(windows, path = "windows/filefacts.rs")]
#[cfg_attr(target_os = "linux", path = "linux/filefacts.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Debug, PartialEq)]
pub enum Signature {
    /// Valid; `catalog` when signed through a Windows catalog rather than
    /// inside the file.
    Signed {
        signer: Option<String>,
        catalog: bool,
    },
    Unsigned,
    /// Signed, but the signature does not hold (tampered, untrusted root,
    /// expired without a timestamp…): Windows' reason.
    Invalid(String),
}

/// What the PE header says about how the file was built.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PeFacts {
    pub machine: &'static str,
    /// Link time, seconds since 1970, unless `reproducible`.
    pub linked: u32,
    /// A reproducible build: `linked` is a hash of the contents, not a time
    /// (most of Windows' own files).
    pub reproducible: bool,
    pub dll: bool,
    pub image_base: u64,
    /// Loaded anywhere (DYNAMIC_BASE).
    pub aslr: bool,
    pub high_entropy: bool,
    /// No execution from data pages (NX_COMPAT).
    pub dep: bool,
    /// Control Flow Guard.
    pub cfg: bool,
    /// Windows refuses to load it unless its signature holds.
    pub force_integrity: bool,
    pub dotnet: bool,
}

#[derive(Clone, Debug)]
pub struct FileFacts {
    pub path: Arc<str>,
    pub size: u64,
    /// Last written, as a FILETIME.
    pub modified: Option<i64>,
    pub sha256: Option<String>,
    pub signature: Signature,
    pub pe: Option<PeFacts>,
    pub took: Duration,
}

/// SHA-256 of the whole file, as VirusTotal and friends know it.
pub fn sha256(path: &str) -> Option<String> {
    sha256_of(std::fs::File::open(path).ok()?)
}
