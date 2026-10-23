// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Reverse DNS for the remote ends of connections, on a few threads of its
//! own: a lookup can take seconds and must never hold up the UI.

use std::net::IpAddr;
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};

#[cfg_attr(windows, path = "windows/resolver.rs")]
#[cfg_attr(target_os = "linux", path = "linux/resolver.rs")]
mod imp;

/// Lookups side by side: one slow answer must not hold up the rest.
const THREADS: usize = 4;

/// What an address is without asking DNS: this machine, the local network.
pub fn local_kind(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) if v4.is_loopback() => Some("this computer"),
        IpAddr::V4(v4) if v4.is_private() || v4.is_link_local() => Some("local network"),
        IpAddr::V4(v4) if v4.is_multicast() => Some("multicast"),
        IpAddr::V4(v4) if v4.is_broadcast() => Some("broadcast"),
        IpAddr::V4(v4) if v4.is_unspecified() => Some("any"),
        IpAddr::V6(v6) if v6.is_loopback() => Some("this computer"),
        IpAddr::V6(v6) if v6.is_unicast_link_local() || v6.is_unique_local() => {
            Some("local network")
        }
        IpAddr::V6(v6) if v6.is_multicast() => Some("multicast"),
        IpAddr::V6(v6) if v6.is_unspecified() => Some("any"),
        _ => None,
    }
}

/// Answers: an address and its name (None: it has none).
pub type Answers = Receiver<(IpAddr, Option<Arc<str>>)>;

/// Looks names up on request; answers (None: it has none) come back on the
/// receiver.
pub struct Resolver {
    tx: Sender<IpAddr>,
}

impl Resolver {
    pub fn start(notify: impl Fn() + Send + Sync + 'static) -> (Self, Answers) {
        imp::init();
        let (tx, rx) = unbounded::<IpAddr>();
        let (out, results) = unbounded();
        let notify = Arc::new(notify);
        for i in 0..THREADS {
            let (rx, out, notify) = (rx.clone(), out.clone(), notify.clone());
            let _ = thread::Builder::new()
                .name(format!("anabolic-dns-{i}"))
                .spawn(move || {
                    crate::hardening::set_current_thread_priority(
                        crate::hardening::ThreadPriority::BelowNormal,
                    );
                    for ip in rx {
                        let name = imp::reverse(ip).map(Arc::from);
                        if out.send((ip, name)).is_err() {
                            return;
                        }
                        notify();
                    }
                });
        }
        (Self { tx }, results)
    }

    pub fn lookup(&self, ip: IpAddr) {
        let _ = self.tx.send(ip);
    }
}
