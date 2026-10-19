//! Everything Anabolic knows about the machine, without any UI.
//!
//! Threads: the collector samples on a fixed tick and publishes immutable
//! snapshots; the enrichment worker fills in slow per-process facts; the
//! action runner executes process control. None of them share a lock with the
//! UI thread.
//!
//! Each module says what it means on every platform; what reads it from
//! the system lives beside it, in `windows/` and `linux/`.

#[cfg(not(any(windows, target_os = "linux")))]
compile_error!("Anabolic runs on Windows and Linux.");

pub mod actions;
pub mod activity;
pub mod collector;
pub mod coretrace;
pub mod cpu;
pub mod disk;
pub mod disktrace;
pub mod diskusage;
pub mod displays;
pub mod dnscache;
pub mod dump;
pub mod ending;
pub mod enrich;
pub mod filefacts;
pub mod foreground;
pub mod frames;
pub mod gpu;
pub mod growth;
pub mod handles;
pub mod hardening;
pub mod history;
pub mod hotkey;
pub mod hwinfo;
pub mod instance;
pub mod latency;
pub mod lineage;
pub mod memory;
pub mod model;
pub mod modules;
pub mod net;
pub mod nettrace;
pub mod placement;
pub mod power;
pub mod recorder;
pub mod resolver;
pub mod scan;
pub mod services;
pub mod sockets;
pub mod startup;
pub mod sysinfo;
pub mod threads;
pub mod topology;
pub mod update;
pub mod waits;

// Windows only: Event Tracing for Windows, the native API, and NT paths,
// version resources and SIDs.
#[cfg(windows)]
#[path = "windows/etw.rs"]
pub mod etw;
#[cfg(windows)]
#[path = "windows/firewall.rs"]
pub mod firewall;
#[cfg(windows)]
#[path = "windows/ntapi.rs"]
pub mod ntapi;
#[cfg(windows)]
#[path = "windows/winfile.rs"]
pub mod winfile;

// Linux only: reading /proc, the desktop's programs and icons, and the
// package manager's records.
#[cfg(target_os = "linux")]
#[path = "linux/apps.rs"]
mod apps;
#[cfg(target_os = "linux")]
#[path = "linux/packages.rs"]
pub mod packages;
#[cfg(target_os = "linux")]
#[path = "linux/procfs.rs"]
mod procfs;

#[cfg_attr(windows, path = "windows/util.rs")]
#[cfg_attr(target_os = "linux", path = "linux/util.rs")]
mod util;

pub use util::random_hex;

pub use collector::{Collector, CollectorConfig, Command};
pub use enrich::{Arch, Enriched, Icon, Integrity, ProcessDetails};
pub use model::*;
