//! Names for remote addresses (reverse DNS), looked up on threads of their
//! own: an address without a name can take seconds to say so.

use std::net::IpAddr;
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use windows::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, GetNameInfoW, IN_ADDR, IN_ADDR_0, IN6_ADDR, IN6_ADDR_0, NI_NAMEREQD,
    SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6, WSADATA, WSAStartup, socklen_t,
};

use crate::util::from_wide;

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

fn reverse(ip: IpAddr) -> Option<String> {
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

/// Answers: an address and its name (None: it has none).
pub type Answers = Receiver<(IpAddr, Option<Arc<str>>)>;

/// Looks names up on request; answers (None: it has none) come back on the
/// receiver.
pub struct Resolver {
    tx: Sender<IpAddr>,
}

impl Resolver {
    pub fn start(notify: impl Fn() + Send + Sync + 'static) -> (Self, Answers) {
        unsafe {
            let mut data = WSADATA::default();
            let _ = WSAStartup(0x0202, &mut data);
        }
        let (tx, rx) = unbounded::<IpAddr>();
        let (out, results) = unbounded();
        let notify = Arc::new(notify);
        for i in 0..THREADS {
            let (rx, out, notify) = (rx.clone(), out.clone(), notify.clone());
            let _ = thread::Builder::new()
                .name(format!("anabolic-dns-{i}"))
                .spawn(move || {
                    crate::hardening::set_current_thread_priority(
                        windows::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL,
                    );
                    for ip in rx {
                        let name = reverse(ip).map(Arc::from);
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

#[cfg(test)]
mod tests {
    use super::*;

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
