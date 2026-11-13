// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Pings on Linux through an unprivileged ICMP socket (what ping uses
//! where net.ipv4.ping_group_range allows the user, as most distributions
//! set it): echo replies come back on it, and a router's "time exceeded"
//! on its error queue (IP_RECVERR). The router from /proc/net/route, the
//! DNS servers from /etc/resolv.conf (or systemd-resolved's own list).

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use crate::netquality::Reply;

pub struct Pinger {
    fd: i32,
    sequence: std::cell::Cell<u16>,
}

unsafe impl Send for Pinger {}

impl Pinger {
    pub fn new() -> Result<Pinger, String> {
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_ICMP) };
        if fd < 0 {
            return Err(
                "this user may not send pings (net.ipv4.ping_group_range does not include it)"
                    .into(),
            );
        }
        let on: libc::c_int = 1;
        unsafe {
            libc::setsockopt(
                fd,
                libc::IPPROTO_IP,
                libc::IP_RECVERR,
                (&raw const on).cast(),
                size_of::<libc::c_int>() as u32,
            );
        }
        Ok(Pinger {
            fd,
            sequence: std::cell::Cell::new(1),
        })
    }

    pub fn send(&self, to: Ipv4Addr, ttl: u8, timeout: Duration) -> Reply {
        let ttl = ttl as libc::c_int;
        let seq = self.sequence.get();
        self.sequence.set(seq.wrapping_add(1));
        // Echo request: type 8; the kernel fills in the id and checksum.
        let mut packet = [0u8; 40];
        packet[0] = 8;
        packet[6..8].copy_from_slice(&seq.to_be_bytes());
        let addr = libc::sockaddr_in {
            sin_family: libc::AF_INET as u16,
            sin_port: 0,
            sin_addr: libc::in_addr {
                s_addr: u32::from_ne_bytes(to.octets()),
            },
            sin_zero: [0; 8],
        };
        unsafe {
            libc::setsockopt(
                self.fd,
                libc::IPPROTO_IP,
                libc::IP_TTL,
                (&raw const ttl).cast(),
                size_of::<libc::c_int>() as u32,
            );
        }
        let started = Instant::now();
        let sent = unsafe {
            libc::sendto(
                self.fd,
                packet.as_ptr().cast(),
                packet.len(),
                0,
                (&raw const addr).cast(),
                size_of::<libc::sockaddr_in>() as u32,
            )
        };
        if sent < 0 {
            return Reply::Lost;
        }
        loop {
            let left = timeout.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Reply::Lost;
            }
            let mut poll = libc::pollfd {
                fd: self.fd,
                events: libc::POLLIN | libc::POLLERR,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll, 1, left.as_millis().max(1) as i32) };
            if ready <= 0 {
                return Reply::Lost;
            }
            let ms = started.elapsed().as_secs_f32() * 1000.0;
            if poll.revents & libc::POLLERR != 0
                && let Some(r) = self.error(ms)
            {
                return r;
            }
            if poll.revents & libc::POLLIN != 0 {
                let mut buf = [0u8; 128];
                let n = unsafe {
                    libc::recv(
                        self.fd,
                        buf.as_mut_ptr().cast(),
                        buf.len(),
                        libc::MSG_DONTWAIT,
                    )
                };
                // An echo reply (type 0) to this request.
                if n >= 8 && buf[0] == 0 && buf[6..8] == seq.to_be_bytes() {
                    return Reply::Echo { ms };
                }
            }
        }
    }

    /// A "time exceeded" or "unreachable" from a router, off the error
    /// queue.
    fn error(&self, ms: f32) -> Option<Reply> {
        let mut data = [0u8; 128];
        let mut control = [0u8; 512];
        let mut from: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        let mut iov = libc::iovec {
            iov_base: data.as_mut_ptr().cast(),
            iov_len: data.len(),
        };
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_name = (&raw mut from).cast();
        msg.msg_namelen = size_of::<libc::sockaddr_in>() as u32;
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = control.len();
        let n =
            unsafe { libc::recvmsg(self.fd, &mut msg, libc::MSG_ERRQUEUE | libc::MSG_DONTWAIT) };
        if n < 0 {
            return None;
        }
        let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        while !cmsg.is_null() {
            let c = unsafe { &*cmsg };
            if c.cmsg_level == libc::IPPROTO_IP && c.cmsg_type == libc::IP_RECVERR {
                let err = unsafe {
                    libc::CMSG_DATA(cmsg)
                        .cast::<libc::sock_extended_err>()
                        .read_unaligned()
                };
                // The offender's address follows the error.
                let offender = unsafe {
                    libc::CMSG_DATA(cmsg)
                        .add(size_of::<libc::sock_extended_err>())
                        .cast::<libc::sockaddr_in>()
                        .read_unaligned()
                };
                let at = Ipv4Addr::from(offender.sin_addr.s_addr.to_ne_bytes());
                if err.ee_origin == libc::SO_EE_ORIGIN_ICMP {
                    return Some(match err.ee_type {
                        11 => Reply::Expired { from: at, ms },
                        3 => Reply::Unreachable { from: at },
                        _ => Reply::Lost,
                    });
                }
            }
            cmsg = unsafe { libc::CMSG_NXTHDR(&msg, cmsg) };
        }
        None
    }
}

impl Drop for Pinger {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

/// The default route's gateway, from /proc/net/route (hex, little-endian).
pub fn gateway() -> Option<Ipv4Addr> {
    let routes = std::fs::read_to_string("/proc/net/route").ok()?;
    routes.lines().skip(1).find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() > 2 && f[1] == "00000000" {
            let g = u32::from_str_radix(f[2], 16).ok()?;
            (g != 0).then(|| Ipv4Addr::from(g.to_le_bytes()))
        } else {
            None
        }
    })
}

/// The DNS servers, from systemd-resolved's list when it runs (its
/// resolv.conf names only its own stub), else resolv.conf.
pub fn dns_servers() -> Vec<Ipv4Addr> {
    let read = |p: &str| {
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().strip_prefix("nameserver"))
            .filter_map(|a| a.trim().parse::<Ipv4Addr>().ok())
            .filter(|a| !a.is_loopback())
            .collect::<Vec<_>>()
    };
    let resolved = read("/run/systemd/resolve/resolv.conf");
    if resolved.is_empty() {
        read("/etc/resolv.conf")
    } else {
        resolved
    }
}
