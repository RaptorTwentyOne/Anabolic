// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Reverse lookups through the C library, which asks whatever the system
//! is set up to ask (systemd-resolved, /etc/hosts, mDNS).

use std::net::IpAddr;

/// Nothing to start on Linux.
pub(super) fn init() {}

pub(super) fn reverse(ip: IpAddr) -> Option<String> {
    let mut name = [0 as libc::c_char; 1025];
    let status = unsafe {
        match ip {
            IpAddr::V4(v4) => {
                let mut addr: libc::sockaddr_in = std::mem::zeroed();
                addr.sin_family = libc::AF_INET as libc::sa_family_t;
                addr.sin_addr.s_addr = u32::from_ne_bytes(v4.octets());
                libc::getnameinfo(
                    (&addr as *const libc::sockaddr_in).cast(),
                    size_of::<libc::sockaddr_in>() as libc::socklen_t,
                    name.as_mut_ptr(),
                    name.len() as libc::socklen_t,
                    std::ptr::null_mut(),
                    0,
                    libc::NI_NAMEREQD,
                )
            }
            IpAddr::V6(v6) => {
                let mut addr: libc::sockaddr_in6 = std::mem::zeroed();
                addr.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                addr.sin6_addr.s6_addr = v6.octets();
                libc::getnameinfo(
                    (&addr as *const libc::sockaddr_in6).cast(),
                    size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                    name.as_mut_ptr(),
                    name.len() as libc::socklen_t,
                    std::ptr::null_mut(),
                    0,
                    libc::NI_NAMEREQD,
                )
            }
        }
    };
    if status != 0 {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    (!name.is_empty()).then_some(name)
}
