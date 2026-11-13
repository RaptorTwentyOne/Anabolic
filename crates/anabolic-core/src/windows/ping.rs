// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Pings on Windows through the ICMP helper (IcmpSendEcho2, as ping.exe
//! and tracert do: no administrator rights needed), the router from the
//! route to the internet (GetBestRoute2), and the DNS servers from the
//! adapters' addresses.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS, HANDLE};
use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_MULTICAST,
    GetAdaptersAddresses, GetBestRoute2, ICMP_ECHO_REPLY, IP_ADAPTER_ADDRESSES_LH,
    IP_OPTION_INFORMATION, IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho2, MIB_IPFORWARD_ROW2,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN, SOCKADDR_INET};

use crate::netquality::Reply;

// IP_STATUS values.
const IP_SUCCESS: u32 = 0;
const IP_DEST_NET_UNREACHABLE: u32 = 11002;
const IP_DEST_HOST_UNREACHABLE: u32 = 11003;
const IP_TTL_EXPIRED_TRANSIT: u32 = 11013;

pub struct Pinger(HANDLE);

// The handle may be used from any one thread at a time.
unsafe impl Send for Pinger {}

impl Pinger {
    pub fn new() -> Result<Pinger, String> {
        unsafe { IcmpCreateFile() }
            .map(Pinger)
            .map_err(|e| e.message())
    }

    pub fn send(&self, to: Ipv4Addr, ttl: u8, timeout: Duration) -> Reply {
        let data = [0x41u8; 32];
        let mut reply = vec![0u8; size_of::<ICMP_ECHO_REPLY>() + data.len() + 64];
        let options = IP_OPTION_INFORMATION {
            Ttl: ttl,
            ..Default::default()
        };
        let started = Instant::now();
        let n = unsafe {
            IcmpSendEcho2(
                self.0,
                None,
                None,
                None,
                u32::from_ne_bytes(to.octets()),
                data.as_ptr().cast(),
                data.len() as u16,
                Some(&options),
                reply.as_mut_ptr().cast(),
                reply.len() as u32,
                timeout.as_millis() as u32,
            )
        };
        // Timed by us: the helper counts whole milliseconds, and a router
        // next door answers in less than one.
        let ms = started.elapsed().as_secs_f32() * 1000.0;
        if n == 0 {
            return Reply::Lost;
        }
        let r = unsafe { reply.as_ptr().cast::<ICMP_ECHO_REPLY>().read_unaligned() };
        let from = Ipv4Addr::from(r.Address.to_ne_bytes());
        match r.Status {
            IP_SUCCESS => Reply::Echo { ms },
            IP_TTL_EXPIRED_TRANSIT => Reply::Expired { from, ms },
            IP_DEST_NET_UNREACHABLE | IP_DEST_HOST_UNREACHABLE => Reply::Unreachable { from },
            _ => Reply::Lost,
        }
    }
}

impl Drop for Pinger {
    fn drop(&mut self) {
        let _ = unsafe { IcmpCloseHandle(self.0) };
    }
}

fn v4(addr: &SOCKADDR_INET) -> Option<Ipv4Addr> {
    unsafe {
        (addr.si_family == AF_INET)
            .then(|| Ipv4Addr::from(addr.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes()))
            .filter(|a| !a.is_unspecified())
    }
}

/// The router: the next hop on the way to the internet.
pub fn gateway() -> Option<Ipv4Addr> {
    let mut dest = SOCKADDR_INET::default();
    dest.Ipv4 = SOCKADDR_IN {
        sin_family: AF_INET,
        ..Default::default()
    };
    dest.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes([1, 1, 1, 1]);
    let mut row = MIB_IPFORWARD_ROW2::default();
    let mut source = SOCKADDR_INET::default();
    let err = unsafe { GetBestRoute2(None, 0, None, &dest, 0, &mut row, &mut source) };
    (err == ERROR_SUCCESS).then(|| v4(&row.NextHop)).flatten()
}

/// The DNS servers of the adapters that are up, in their order.
pub fn dns_servers() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_INCLUDE_GATEWAYS;
    let mut size = 16 * 1024u32;
    let mut buf: Vec<u64>;
    loop {
        buf = vec![0u64; size as usize / 8 + 1];
        let err = unsafe {
            GetAdaptersAddresses(
                AF_INET.0 as u32,
                flags,
                None,
                Some(buf.as_mut_ptr().cast()),
                &mut size,
            )
        };
        if err == ERROR_BUFFER_OVERFLOW.0 {
            continue;
        }
        if err != ERROR_SUCCESS.0 {
            return out;
        }
        break;
    }
    let mut at = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    while !at.is_null() {
        let a = unsafe { &*at };
        if a.OperStatus == IfOperStatusUp && !a.FirstGatewayAddress.is_null() {
            let mut dns = a.FirstDnsServerAddress;
            while !dns.is_null() {
                let d = unsafe { &*dns };
                let sa = d.Address.lpSockaddr;
                if !sa.is_null() && unsafe { (*sa).sa_family } == AF_INET {
                    let sin = unsafe { &*sa.cast::<SOCKADDR_IN>() };
                    let ip = Ipv4Addr::from(unsafe { sin.sin_addr.S_un.S_addr }.to_ne_bytes());
                    if !out.contains(&ip) {
                        out.push(ip);
                    }
                }
                dns = d.Next;
            }
        }
        at = a.Next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pings_this_machine() {
        let p = Pinger::new().unwrap();
        assert!(matches!(
            p.send(Ipv4Addr::LOCALHOST, 64, Duration::from_secs(1)),
            Reply::Echo { .. }
        ));
        println!("router {:?}, DNS {:?}", gateway(), dns_servers());
    }
}
