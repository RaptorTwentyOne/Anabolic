// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Names for addresses from Windows' DNS cache, what `ipconfig /displaydns`
//! lists: a connection opened before Anabolic started was looked up then,
//! and the answer is often still cached. Read from the cache only; nothing
//! goes out on the network.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use windows::Win32::NetworkManagement::Dns::{
    DNS_QUERY_NO_WIRE_QUERY, DNS_RECORDA, DNS_RECORDW, DNS_TYPE, DNS_TYPE_A, DNS_TYPE_AAAA,
    DNS_TYPE_CNAME, DnsFree, DnsFreeFlat, DnsFreeRecordList, DnsQuery_W,
};
use windows::core::{BOOL, PCWSTR, PWSTR};

/// One name in the cache (the shape `ipconfig` relies on).
#[repr(C)]
struct CacheEntry {
    next: *mut CacheEntry,
    name: PWSTR,
    kind: u16,
    data_length: u16,
    flags: u32,
}

windows::core::link!("dnsapi.dll" "system" fn DnsGetCacheDataTable(entries: *mut *mut CacheEntry) -> BOOL);

/// The names in the cache.
fn cached_names() -> Vec<String> {
    let mut first: *mut CacheEntry = std::ptr::null_mut();
    if !unsafe { DnsGetCacheDataTable(&mut first) }.as_bool() {
        return Vec::new();
    }
    let mut names = Vec::new();
    let mut at = first;
    while !at.is_null() {
        unsafe {
            let entry = &*at;
            if let Ok(name) = entry.name.to_string()
                && !names.contains(&name)
            {
                names.push(name);
            }
            let next = entry.next;
            DnsFree(Some(entry.name.0.cast()), DnsFreeFlat);
            DnsFree(Some(at.cast()), DnsFreeFlat);
            at = next;
        }
    }
    names
}

/// The cached answer for `name`: its addresses, and whether it is an alias
/// (a CNAME) of another name.
fn cached(name: &str, kind: DNS_TYPE) -> (Vec<IpAddr>, bool) {
    let wide = crate::util::wide(name);
    let mut first: *mut DNS_RECORDA = std::ptr::null_mut();
    let err = unsafe {
        DnsQuery_W(
            PCWSTR(wide.as_ptr()),
            kind,
            DNS_QUERY_NO_WIRE_QUERY,
            None,
            &mut first,
            None,
        )
    };
    if err.0 != 0 || first.is_null() {
        return (Vec::new(), false);
    }
    let mut addresses = Vec::new();
    let mut alias = false;
    let mut at = first.cast::<DNS_RECORDW>();
    while !at.is_null() {
        let r = unsafe { &*at };
        match DNS_TYPE(r.wType) {
            DNS_TYPE_A => addresses.push(IpAddr::V4(Ipv4Addr::from(
                unsafe { r.Data.A.IpAddress }.to_ne_bytes(),
            ))),
            DNS_TYPE_AAAA => addresses.push(IpAddr::V6(Ipv6Addr::from(unsafe {
                r.Data.AAAA.Ip6Address.IP6Byte
            }))),
            DNS_TYPE_CNAME => alias = true,
            _ => {}
        }
        at = r.pNext;
    }
    unsafe { DnsFree(Some(first.cast_const().cast()), DnsFreeRecordList) };
    (addresses, alias)
}

/// Address -> the name it was looked up by, from the cache. Aliases win:
/// "www.microsoft.com" says more than the CDN name it points to.
pub fn names() -> HashMap<IpAddr, Arc<str>> {
    let mut by_alias = Vec::new();
    let mut direct = Vec::new();
    for name in cached_names() {
        for kind in [DNS_TYPE_A, DNS_TYPE_AAAA] {
            let (addresses, alias) = cached(&name, kind);
            let list = if alias { &mut by_alias } else { &mut direct };
            for ip in addresses {
                list.push((ip, name.clone()));
            }
        }
    }
    let mut map: HashMap<IpAddr, Arc<str>> = HashMap::new();
    for (ip, name) in by_alias.into_iter().chain(direct) {
        map.entry(ip).or_insert_with(|| Arc::from(name.as_str()));
    }
    map
}

#[cfg(test)]
mod tests {
    /// Answers in the cache live as long as their TTL, seconds for a CDN,
    /// so what is in it at test time is anyone's guess; this walks and
    /// frees whatever is there.
    #[test]
    fn reads_the_cache() {
        use std::net::ToSocketAddrs;
        let _ = "www.microsoft.com:443".to_socket_addrs();
        assert!(!super::cached_names().is_empty());
        let _ = super::names();
    }
}
