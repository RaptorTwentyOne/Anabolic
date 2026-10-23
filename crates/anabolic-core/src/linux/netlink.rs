// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Netlink, the kernel's message interface: the socket tables with their
//! counters (socket diagnostics, what `ss` reads), closing a connection
//! through them, and the process events connector (fork, exec, exit).

use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use crate::nettrace::Proto;
use crate::sockets::ip6;

const NETLINK_SOCK_DIAG: i32 = 4;
const NETLINK_CONNECTOR: i32 = 11;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const SOCK_DIAG_BY_FAMILY: u16 = 20;
const SOCK_DESTROY: u16 = 21;
const NLM_F_REQUEST: u16 = 0x1;
const NLM_F_ACK: u16 = 0x4;
const NLM_F_DUMP: u16 = 0x300;
/// The attribute carrying `struct tcp_info`.
const INET_DIAG_INFO: u16 = 2;
/// The connector's process events: index and value.
const CN_IDX_PROC: u32 = 1;
const CN_VAL_PROC: u32 = 1;
const PROC_CN_MCAST_LISTEN: u32 = 1;

/// A netlink socket of one family.
struct Netlink(OwnedFd);

impl Netlink {
    fn open(protocol: i32, flags: i32) -> io::Result<Self> {
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | flags,
                protocol,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    fn send(&self, msg: &[u8]) -> io::Result<()> {
        let sent = unsafe { libc::send(self.0.as_raw_fd(), msg.as_ptr().cast(), msg.len(), 0) };
        if sent < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// One datagram; Ok(0) when none waits on a non-blocking socket.
    fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        let got = unsafe { libc::recv(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
        if got < 0 {
            let e = io::Error::last_os_error();
            return match e.kind() {
                io::ErrorKind::WouldBlock => Ok(0),
                _ => Err(e),
            };
        }
        Ok(got as usize)
    }
}

/// A netlink message: its header, then `body`.
fn message(kind: u16, flags: u16, body: &[u8]) -> Vec<u8> {
    let len = 16 + body.len();
    let mut msg = Vec::with_capacity(len);
    msg.extend_from_slice(&(len as u32).to_ne_bytes());
    msg.extend_from_slice(&kind.to_ne_bytes());
    msg.extend_from_slice(&flags.to_ne_bytes());
    msg.extend_from_slice(&1u32.to_ne_bytes());
    msg.extend_from_slice(&0u32.to_ne_bytes());
    msg.extend_from_slice(body);
    msg
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

/// The netlink messages in one datagram: (type, payload).
fn messages(buf: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    let mut at = 0;
    std::iter::from_fn(move || {
        let len = u32_at(buf, at)? as usize;
        if len < 16 || at + len > buf.len() {
            return None;
        }
        let kind = u16_at(buf, at + 4)?;
        let payload = &buf[at + 16..at + len];
        at += len.div_ceil(4) * 4;
        Some((kind, payload))
    })
}

/// One socket as the diagnostics interface reports it.
#[derive(Clone, Debug)]
pub struct DiagSocket {
    /// The kernel's TCP state number (1 established … 10 listening); UDP
    /// sockets report 7 (unconnected) or 1 (connected).
    pub state: u8,
    pub local: (IpAddr, u16),
    pub remote: (IpAddr, u16),
    pub uid: u32,
    /// 0 for a socket no process holds any more (TIME_WAIT and the like).
    pub inode: u64,
    /// The kernel's own id for it: unique while the system runs.
    pub cookie: u64,
    /// TCP only: what the other end acknowledged having received, and
    /// what arrived from it, in bytes since the connection began (the
    /// opening handshake counts one byte sent).
    pub sent: Option<u64>,
    pub received: Option<u64>,
}

/// An address and port.
type Endpoint = (IpAddr, u16);

/// The inet_diag_sockid in a request or reply: local and remote end, and
/// the cookie.
fn sock_id(bytes: &[u8], family: u8) -> Option<(Endpoint, Endpoint, u64)> {
    let port = |at: usize| Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let address = |at: usize| -> Option<IpAddr> {
        if family == libc::AF_INET as u8 {
            let b: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
            Some(IpAddr::V4(Ipv4Addr::from(b)))
        } else {
            Some(ip6(bytes.get(at..at + 16)?.try_into().ok()?))
        }
    };
    let cookie = u32_at(bytes, 40)? as u64 | (u32_at(bytes, 44)? as u64) << 32;
    Some(((address(4)?, port(0)?), (address(20)?, port(2)?), cookie))
}

/// The bytes acknowledged and received, from `struct tcp_info`.
fn tcp_counters(info: &[u8]) -> (Option<u64>, Option<u64>) {
    // tcpi_bytes_acked at 120, tcpi_bytes_received at 128 (Linux 4.1+).
    (u64_at(info, 120), u64_at(info, 128))
}

fn parse_socket(proto: Proto, msg: &[u8]) -> Option<DiagSocket> {
    // inet_diag_msg: family, state, timer, retrans, id (48), expires,
    // rqueue, wqueue, uid, inode; then attributes.
    let family = *msg.first()?;
    let state = *msg.get(1)?;
    let (local, remote, cookie) = sock_id(msg.get(4..52)?, family)?;
    let uid = u32_at(msg, 64)?;
    let inode = u32_at(msg, 68)? as u64;
    let (mut sent, mut received) = (None, None);
    let mut at = 72;
    while let (Some(len), Some(kind)) = (u16_at(msg, at), u16_at(msg, at + 2)) {
        let len = len as usize;
        if len < 4 || at + len > msg.len() {
            break;
        }
        if kind == INET_DIAG_INFO && proto == Proto::Tcp {
            (sent, received) = tcp_counters(&msg[at + 4..at + len]);
        }
        at += len.div_ceil(4) * 4;
    }
    Some(DiagSocket {
        state,
        local,
        remote,
        uid,
        inode,
        cookie,
        sent,
        received,
    })
}

/// Every socket of one protocol, IPv4 and IPv6, with TCP's byte counters.
/// Anyone may ask: the kernel answers for every user's sockets.
pub fn dump(proto: Proto) -> io::Result<Vec<DiagSocket>> {
    let link = Netlink::open(NETLINK_SOCK_DIAG, 0)?;
    let protocol = match proto {
        Proto::Tcp => libc::IPPROTO_TCP,
        Proto::Udp => libc::IPPROTO_UDP,
    } as u8;
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    for family in [libc::AF_INET as u8, libc::AF_INET6 as u8] {
        // inet_diag_req_v2: family, protocol, extensions, pad, states, id.
        let mut req = [0u8; 56];
        req[0] = family;
        req[1] = protocol;
        req[2] = 1 << (INET_DIAG_INFO - 1);
        req[4..8].copy_from_slice(&u32::MAX.to_ne_bytes());
        link.send(&message(
            SOCK_DIAG_BY_FAMILY,
            NLM_F_REQUEST | NLM_F_DUMP,
            &req,
        ))?;
        'replies: loop {
            let n = link.recv(&mut buf)?;
            if n == 0 {
                break;
            }
            for (kind, payload) in messages(&buf[..n]) {
                match kind {
                    NLMSG_DONE => break 'replies,
                    NLMSG_ERROR => {
                        let errno = -(u32_at(payload, 0).unwrap_or(0) as i32);
                        // No IPv6 in this kernel: nothing to list.
                        if family == libc::AF_INET6 as u8 {
                            break 'replies;
                        }
                        return Err(io::Error::from_raw_os_error(errno));
                    }
                    SOCK_DIAG_BY_FAMILY => out.extend(parse_socket(proto, payload)),
                    _ => {}
                }
            }
        }
    }
    Ok(out)
}

/// Local (Unix) sockets and the socket at their other end, by inode: who a
/// program talks to over its local connections.
pub fn unix_peers() -> io::Result<std::collections::HashMap<u64, u64>> {
    const UDIAG_SHOW_PEER: u32 = 0x4;
    const UNIX_DIAG_PEER: u16 = 2;
    let link = Netlink::open(NETLINK_SOCK_DIAG, 0)?;
    // unix_diag_req: family, protocol, pad, states, inode, show, cookie.
    let mut req = [0u8; 24];
    req[0] = libc::AF_UNIX as u8;
    req[4..8].copy_from_slice(&u32::MAX.to_ne_bytes());
    req[12..16].copy_from_slice(&UDIAG_SHOW_PEER.to_ne_bytes());
    link.send(&message(
        SOCK_DIAG_BY_FAMILY,
        NLM_F_REQUEST | NLM_F_DUMP,
        &req,
    ))?;
    let mut peers = std::collections::HashMap::new();
    let mut buf = vec![0u8; 64 * 1024];
    'replies: loop {
        let n = link.recv(&mut buf)?;
        if n == 0 {
            break;
        }
        for (kind, msg) in messages(&buf[..n]) {
            match kind {
                NLMSG_DONE => break 'replies,
                NLMSG_ERROR => {
                    let errno = -(u32_at(msg, 0).unwrap_or(0) as i32);
                    return Err(io::Error::from_raw_os_error(errno));
                }
                _ => {}
            }
            // unix_diag_msg: family, type, state, pad, inode, cookie;
            // then attributes.
            let Some(inode) = u32_at(msg, 4) else {
                continue;
            };
            let mut at = 16;
            while let (Some(len), Some(attr)) = (u16_at(msg, at), u16_at(msg, at + 2)) {
                let len = len as usize;
                if len < 4 || at + len > msg.len() {
                    break;
                }
                if attr == UNIX_DIAG_PEER
                    && let Some(peer) = u32_at(msg, at + 4)
                {
                    peers.insert(inode as u64, peer as u64);
                }
                at += len.div_ceil(4) * 4;
            }
        }
    }
    Ok(peers)
}

/// Resets one TCP connection at once, as `ss -K` does: the program
/// holding it sees it reset. An IPv4 connection may sit on an IPv6 socket
/// (v4-mapped), so both are tried. Needs cap_net_admin.
pub fn destroy(local: (IpAddr, u16), remote: (IpAddr, u16)) -> io::Result<()> {
    let mapped = |ip: IpAddr| match ip {
        IpAddr::V4(v4) => IpAddr::V6(v4.to_ipv6_mapped()),
        v6 => v6,
    };
    let as_v6 = || {
        destroy_in(
            libc::AF_INET6,
            (mapped(local.0), local.1),
            (mapped(remote.0), remote.1),
        )
    };
    match (local.0, remote.0) {
        (IpAddr::V4(_), IpAddr::V4(_)) => match destroy_in(libc::AF_INET, local, remote) {
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => as_v6(),
            done => done,
        },
        _ => as_v6(),
    }
}

fn destroy_in(family: i32, local: (IpAddr, u16), remote: (IpAddr, u16)) -> io::Result<()> {
    let link = Netlink::open(NETLINK_SOCK_DIAG, 0)?;
    let mut req = [0u8; 56];
    req[0] = family as u8;
    req[1] = libc::IPPROTO_TCP as u8;
    req[4..8].copy_from_slice(&u32::MAX.to_ne_bytes());
    let id = &mut req[8..];
    id[0..2].copy_from_slice(&local.1.to_be_bytes());
    id[2..4].copy_from_slice(&remote.1.to_be_bytes());
    for (at, ip) in [(4, local.0), (20, remote.0)] {
        match ip {
            IpAddr::V4(v4) => id[at..at + 4].copy_from_slice(&v4.octets()),
            IpAddr::V6(v6) => id[at..at + 16].copy_from_slice(&v6.octets()),
        }
    }
    // No cookie: match by address.
    id[40..48].copy_from_slice(&[0xFF; 8]);
    link.send(&message(SOCK_DESTROY, NLM_F_REQUEST | NLM_F_ACK, &req))?;
    let mut reply = [0u8; 256];
    let n = link.recv(&mut reply)?;
    for (kind, payload) in messages(&reply[..n]) {
        if kind == NLMSG_ERROR {
            return match -(u32_at(payload, 0).unwrap_or(0) as i32) {
                0 => Ok(()),
                errno => Err(io::Error::from_raw_os_error(errno)),
            };
        }
    }
    Err(io::Error::other("the kernel did not answer"))
}

/// What the process events connector reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcEvent {
    /// A new process (`pid` = its thread group) or thread.
    Fork { parent: u32, pid: u32, tid: u32 },
    /// A process started a new program.
    Exec { pid: u32 },
    /// A thread ended; the process with it when `tid == pid`. `status` as
    /// wait(2) gives it.
    Exit { pid: u32, tid: u32, status: u32 },
}

/// The kernel's process events, as they happen. Anyone may listen since
/// Linux 6.6; before, only with cap_net_admin.
pub struct ProcEvents(Netlink);

impl ProcEvents {
    pub fn open() -> io::Result<Self> {
        let link = Netlink::open(NETLINK_CONNECTOR, libc::SOCK_NONBLOCK)?;
        let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        addr.nl_groups = CN_IDX_PROC;
        let bound = unsafe {
            libc::bind(
                link.0.as_raw_fd(),
                (&addr as *const libc::sockaddr_nl).cast(),
                size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if bound != 0 {
            return Err(io::Error::last_os_error());
        }
        // Every thread's start and end comes too, and the reader drains
        // once a refresh: room for a busy second (the kernel caps it at
        // net.core.rmem_max unless allowed more).
        let room: libc::c_int = 4 << 20;
        for option in [libc::SO_RCVBUFFORCE, libc::SO_RCVBUF] {
            let set = unsafe {
                libc::setsockopt(
                    link.0.as_raw_fd(),
                    libc::SOL_SOCKET,
                    option,
                    (&room as *const libc::c_int).cast(),
                    size_of::<libc::c_int>() as libc::socklen_t,
                )
            };
            if set == 0 {
                break;
            }
        }
        // cn_msg: id (idx, val), seq, ack, len, flags; then the operation.
        let mut body = Vec::with_capacity(24);
        body.extend_from_slice(&CN_IDX_PROC.to_ne_bytes());
        body.extend_from_slice(&CN_VAL_PROC.to_ne_bytes());
        body.extend_from_slice(&0u32.to_ne_bytes());
        body.extend_from_slice(&0u32.to_ne_bytes());
        body.extend_from_slice(&4u16.to_ne_bytes());
        body.extend_from_slice(&0u16.to_ne_bytes());
        body.extend_from_slice(&PROC_CN_MCAST_LISTEN.to_ne_bytes());
        link.send(&message(NLMSG_DONE, 0, &body))?;
        Ok(Self(link))
    }

    /// The file to wait on for the next event.
    pub fn fd(&self) -> i32 {
        self.0.0.as_raw_fd()
    }

    /// Every event waiting now, without blocking. Returns whether some were
    /// lost (the reader fell behind and the kernel dropped them).
    pub fn drain(&self, out: &mut Vec<ProcEvent>) -> bool {
        let mut buf = [0u8; 8192];
        let mut lost = false;
        loop {
            let n = match self.0.recv(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.raw_os_error() == Some(libc::ENOBUFS) => {
                    lost = true;
                    continue;
                }
                Err(_) => break,
            };
            for (kind, payload) in messages(&buf[..n]) {
                if kind == NLMSG_DONE
                    && let Some(event) = parse_proc_event(payload.get(20..).unwrap_or(&[]))
                {
                    out.push(event);
                }
            }
        }
        lost
    }
}

/// `struct proc_event`: what, cpu, timestamp, then the event's own data.
fn parse_proc_event(d: &[u8]) -> Option<ProcEvent> {
    const FORK: u32 = 0x1;
    const EXEC: u32 = 0x2;
    const EXIT: u32 = 0x8000_0000;
    let at = |i: usize| u32_at(d, 16 + i * 4);
    Some(match u32_at(d, 0)? {
        FORK => ProcEvent::Fork {
            parent: at(1)?,
            tid: at(2)?,
            pid: at(3)?,
        },
        EXEC => ProcEvent::Exec { pid: at(1)? },
        EXIT => ProcEvent::Exit {
            tid: at(0)?,
            pid: at(1)?,
            status: at(2)?,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn counts_the_bytes_of_a_local_connection() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        client.write_all(&[7u8; 100_000]).unwrap();
        let mut got = vec![0u8; 100_000];
        server.read_exact(&mut got).unwrap();
        let sockets = dump(Proto::Tcp).expect("socket diagnostics");
        let mine = sockets
            .iter()
            .find(|s| s.local.1 == client.local_addr().unwrap().port() && s.remote.1 == port)
            .expect("the client's socket is listed");
        // The handshake's SYN counts one byte.
        assert_eq!(mine.sent, Some(100_001));
        assert_eq!(mine.state, 1);
        assert_ne!(mine.inode, 0);
        let other = sockets
            .iter()
            .find(|s| s.local.1 == port && s.remote.1 == mine.local.1)
            .expect("the server's end is listed");
        assert_eq!(other.received, Some(100_000));
        assert!(sockets.iter().any(|s| s.local.1 == port && s.state == 10));
    }

    #[test]
    fn lists_udp_sockets() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        let sockets = dump(Proto::Udp).unwrap();
        let mine = sockets.iter().find(|s| s.local.1 == port).expect("listed");
        assert_eq!(mine.sent, None);
    }

    /// Kernels before 6.6 let only cap_net_admin listen: refused politely
    /// there. Elsewhere a child started now is heard starting and ending.
    #[test]
    fn process_events_are_heard_or_refused() {
        let events = match ProcEvents::open() {
            Ok(events) => events,
            Err(e) => {
                assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
                return;
            }
        };
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        let started = std::time::Instant::now();
        let mut seen = Vec::new();
        while started.elapsed() < std::time::Duration::from_secs(3) {
            events.drain(&mut seen);
            if seen.contains(&ProcEvent::Exit {
                pid,
                tid: pid,
                status: 0,
            }) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(seen.contains(&ProcEvent::Exec { pid }), "{seen:?}");
        assert!(
            seen.iter().any(
                |e| matches!(e, ProcEvent::Fork { pid: p, tid, .. } if *p == pid && *tid == pid)
            )
        );
    }

    #[test]
    fn parses_process_events() {
        let mut d = vec![0u8; 40];
        d[0..4].copy_from_slice(&1u32.to_ne_bytes());
        for (i, v) in [10u32, 10, 20, 20].iter().enumerate() {
            d[16 + i * 4..20 + i * 4].copy_from_slice(&v.to_ne_bytes());
        }
        assert_eq!(
            parse_proc_event(&d),
            Some(ProcEvent::Fork {
                parent: 10,
                pid: 20,
                tid: 20
            })
        );
    }
}
