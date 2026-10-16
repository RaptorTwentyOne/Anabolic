//! Everything Anabolic knows about the machine, without any UI.
//!
//! Threads: the collector samples on a fixed tick and publishes immutable
//! snapshots; the enrichment worker fills in slow per-process facts; the
//! action runner executes process control. None of them share a lock with the
//! UI thread.

#![cfg(windows)]

pub mod actions;
pub mod collector;
pub mod coretrace;
pub mod cpu;
pub mod disk;
pub mod disktrace;
pub mod diskusage;
pub mod dnscache;
pub mod dump;
pub mod ending;
pub mod enrich;
pub mod etw;
pub mod filefacts;
pub mod firewall;
pub mod foreground;
pub mod frames;
pub mod gpu;
pub mod growth;
pub mod handles;
pub mod hardening;
pub mod history;
pub mod hwinfo;
pub mod instance;
pub mod latency;
pub mod memory;
pub mod model;
pub mod modules;
pub mod net;
pub mod nettrace;
pub mod ntapi;
pub mod power;
pub mod recorder;
pub mod resolver;
pub mod scan;
pub mod services;
pub mod sockets;
pub mod startup;
pub mod sysinfo;
pub mod taskmgr;
pub mod threads;
pub mod topology;
mod util;
pub mod waits;
pub mod winfile;

pub use collector::{Collector, CollectorConfig, Command};
pub use enrich::{Arch, Enriched, Icon, Integrity, ProcessDetails};
pub use model::*;
