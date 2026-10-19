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
    /// Linux: put in place by the system's package manager, which vouches
    /// for it the way a signature does. `verified` when the manager's
    /// record of the file's hash was compared with the file (pacman keeps
    /// one): still as installed, or changed since.
    Package {
        manager: &'static str,
        name: String,
        version: String,
        verified: Option<bool>,
    },
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

/// What the ELF headers say about how a Linux program or library was
/// built, and the exploit mitigations it was built with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ElfFacts {
    pub machine: &'static str,
    /// A shared library rather than a program.
    pub library: bool,
    /// Needs no shared libraries: everything is linked in.
    pub static_linked: bool,
    /// A program loaded at a random address (position independent).
    pub pie: bool,
    /// Its relocations turn read-only once loaded (RELRO), all of them
    /// before it starts (`bind_now`: full RELRO).
    pub relro: bool,
    pub bind_now: bool,
    /// Its stack is not executable.
    pub nx_stack: bool,
    /// Stack canaries and _FORTIFY_SOURCE, as its symbols show them.
    pub stack_protector: bool,
    pub fortify: bool,
    /// Intel CET: indirect branch tracking and shadow stacks.
    pub cet: bool,
    pub build_id: Option<String>,
    /// Without its own symbol table (debug symbols elsewhere, or none).
    pub stripped: bool,
    /// The dynamic loader it asks for.
    pub interpreter: Option<String>,
}

#[derive(Clone, Debug)]
pub struct FileFacts {
    pub path: Arc<str>,
    pub size: u64,
    /// Last written, as a FILETIME.
    pub modified: Option<i64>,
    pub sha256: Option<String>,
    pub signature: Signature,
    /// A Windows program file.
    pub pe: Option<PeFacts>,
    /// A Linux one.
    pub elf: Option<ElfFacts>,
    pub took: Duration,
}

/// SHA-256 of the whole file, as VirusTotal and friends know it.
pub fn sha256(path: &str) -> Option<String> {
    sha256_of(std::fs::File::open(path).ok()?)
}
