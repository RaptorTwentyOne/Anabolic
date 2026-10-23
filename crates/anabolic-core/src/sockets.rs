// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Every TCP and UDP socket with the process that owns it: the open
//! connections and the ports listening for others, from the system's
//! socket tables. No process is opened.

use crate::nettrace::Proto;
use std::net::{IpAddr, Ipv6Addr};

#[cfg_attr(windows, path = "windows/sockets.rs")]
#[cfg_attr(target_os = "linux", path = "linux/sockets.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
    DeleteTcb,
}

impl TcpState {
    pub fn label(self) -> &'static str {
        match self {
            TcpState::Closed => "Closed",
            TcpState::Listen => "Listening",
            TcpState::SynSent => "Connecting",
            TcpState::SynReceived => "Accepting",
            TcpState::Established => "Open",
            TcpState::FinWait1 | TcpState::FinWait2 | TcpState::Closing | TcpState::LastAck => {
                "Closing"
            }
            // The other end hung up; this program has not yet.
            TcpState::CloseWait => "Peer closed",
            TcpState::TimeWait => "Closed, waiting",
            TcpState::DeleteTcb => "Deleting",
        }
    }

    /// Carries data now, or is about to.
    pub fn live(self) -> bool {
        matches!(
            self,
            TcpState::Established | TcpState::SynSent | TcpState::SynReceived
        )
    }
}

/// One endpoint from the tables.
#[derive(Clone, Debug)]
pub struct Socket {
    pub pid: u32,
    pub proto: Proto,
    pub local: IpAddr,
    pub local_port: u16,
    /// The other end; None for a listening TCP port or any UDP socket (UDP
    /// has no other end of its own).
    pub remote: Option<(IpAddr, u16)>,
    /// TCP only.
    pub state: Option<TcpState>,
}

impl Socket {
    /// Waiting for others to connect: a listening TCP port, a bound UDP one.
    pub fn listening(&self) -> bool {
        self.state == Some(TcpState::Listen) || self.proto == Proto::Udp
    }
}

/// An IPv6 address, or the IPv4 one a dual-stack socket maps into it
/// (::ffff:a.b.c.d), so the same peer is spelled the same way everywhere.
pub fn ip6(bytes: [u8; 16]) -> IpAddr {
    let v6 = Ipv6Addr::from(bytes);
    v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4)
}
