// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Network traffic per process and per connection, process starts and
//! stops, and the names programs look up, from the system's own accounting.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

#[cfg_attr(windows, path = "windows/nettrace.rs")]
#[cfg_attr(target_os = "linux", path = "linux/nettrace.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Proto {
    Tcp,
    Udp,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ConnKey {
    pub pid: u32,
    pub proto: Proto,
    pub remote: IpAddr,
    pub remote_port: u16,
    pub local_port: u16,
}

/// One name a program looked up.
#[derive(Clone, Debug)]
pub struct DnsLookup {
    pub at: std::time::SystemTime,
    pub pid: u32,
    pub name: Arc<str>,
    /// The record type asked for: 1 = A (IPv4), 28 = AAAA (IPv6), ...
    pub kind: u16,
    /// 0, or the Windows error: 9003 no such name, 1460 timed out, ...
    pub status: u32,
    pub addresses: Vec<IpAddr>,
    /// The names it was an alias of on the way (CNAMEs).
    pub aliases: Vec<Arc<str>>,
}

impl DnsLookup {
    pub fn kind_label(&self) -> String {
        match self.kind {
            1 => "A".into(),
            5 => "CNAME".into(),
            12 => "PTR".into(),
            15 => "MX".into(),
            16 => "TXT".into(),
            28 => "AAAA".into(),
            33 => "SRV".into(),
            65 => "HTTPS".into(),
            other => other.to_string(),
        }
    }

    pub fn status_label(&self) -> String {
        match self.status {
            0 if self.addresses.is_empty() && self.aliases.is_empty() => "no records".into(),
            0 => "answered".into(),
            9003 => "no such name".into(),
            9501 => "no records".into(),
            1460 => "timed out".into(),
            9002 => "server failure".into(),
            1214 => "invalid name".into(),
            other => format!("error {other}"),
        }
    }
}

/// A process started.
#[derive(Clone, Debug)]
pub struct ProcessStart {
    pub pid: u32,
    /// FILETIME, as in ProcKey.
    pub create_time: i64,
    pub ppid: u32,
    pub elevated: bool,
    /// The NT path of its file (\Device\HarddiskVolumeN\...).
    pub image: String,
}

/// A process exited.
#[derive(Clone, Copy, Debug)]
pub struct ProcessStop {
    pub pid: u32,
    pub create_time: i64,
    pub exit_time: i64,
    pub exit_code: u32,
}

/// Bytes seen since the last `take`.
#[derive(Default, Debug)]
pub struct Traffic {
    /// pid -> (received, sent)
    pub per_pid: HashMap<u32, (u64, u64)>,
    pub per_conn: HashMap<ConnKey, (u64, u64)>,
    pub lookups: Vec<DnsLookup>,
    pub starts: Vec<ProcessStart>,
    pub stops: Vec<ProcessStop>,
}
