// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Names for remote addresses (reverse DNS), looked up on threads of their
//! own: an address without a name can take seconds to say so.

use std::net::IpAddr;

use windows::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, GetNameInfoW, IN_ADDR, IN_ADDR_0, IN6_ADDR, IN6_ADDR_0, NI_NAMEREQD,
    SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6, WSADATA, WSAStartup, socklen_t,
};

use crate::util::from_wide;

/// Winsock wants to be started before the first lookup.
pub(super) fn init() {
    unsafe {
        let mut data = WSADATA::default();
        let _ = WSAStartup(0x0202, &mut data);
    }
}

pub(super) fn reverse(ip: IpAddr) -> Option<String> {
    let mut name = [0u16; 1025];
    let status = unsafe {
        match ip {
            IpAddr::V4(v4) => {
                let addr = SOCKADDR_IN {
                    sin_family: AF_INET,
                    sin_port: 0,
                    sin_addr: IN_ADDR {
                        S_un: IN_ADDR_0 {
                            S_addr: u32::from_ne_bytes(v4.octets()),
                        },
                    },
                    sin_zero: [0; 8],
                };
                GetNameInfoW(
                    (&addr as *const SOCKADDR_IN).cast::<SOCKADDR>(),
                    socklen_t(size_of::<SOCKADDR_IN>() as i32),
                    Some(&mut name),
                    None,
                    NI_NAMEREQD as i32,
                )
            }
            IpAddr::V6(v6) => {
                let addr = SOCKADDR_IN6 {
                    sin6_family: AF_INET6,
                    sin6_addr: IN6_ADDR {
                        u: IN6_ADDR_0 { Byte: v6.octets() },
                    },
                    ..Default::default()
                };
                GetNameInfoW(
                    (&addr as *const SOCKADDR_IN6).cast::<SOCKADDR>(),
                    socklen_t(size_of::<SOCKADDR_IN6>() as i32),
                    Some(&mut name),
                    None,
                    NI_NAMEREQD as i32,
                )
            }
        }
    };
    (status == 0)
        .then(|| from_wide(&name))
        .filter(|n| !n.is_empty())
}

#[cfg(test)]
mod tests {
    use crate::resolver::*;

    #[test]
    fn local_addresses_need_no_lookup() {
        assert_eq!(
            local_kind("127.0.0.1".parse().unwrap()),
            Some("this computer")
        );
        assert_eq!(
            local_kind("192.168.1.10".parse().unwrap()),
            Some("local network")
        );
        assert_eq!(
            local_kind("fe80::1".parse().unwrap()),
            Some("local network")
        );
        assert_eq!(local_kind("8.8.8.8".parse().unwrap()), None);
    }

    #[test]
    fn names_a_public_resolver() {
        let (resolver, results) = Resolver::start(|| {});
        resolver.lookup("8.8.8.8".parse().unwrap());
        let (ip, name) = results
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("an answer");
        assert_eq!(ip, "8.8.8.8".parse::<IpAddr>().unwrap());
        // Offline machines answer None; online ones dns.google.
        if let Some(name) = name {
            assert!(name.contains("google"), "{name}");
        }
    }
}
