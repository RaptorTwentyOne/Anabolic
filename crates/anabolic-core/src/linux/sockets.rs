//! Sockets from the kernel's tables in /proc/net, matched to the processes
//! holding them through their open files; closing a connection through
//! the socket diagnostics interface (what `ss -K` does).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use super::*;

/// Which process holds each socket (by inode): finding out reads the name
/// of every open file of every process, so it is done only when a socket
/// nobody is known to hold turns up, and not more often than the caller
/// allows.
struct Owners {
    by_inode: HashMap<u64, u32>,
    scanned: Option<Instant>,
}

static OWNERS: LazyLock<Mutex<Owners>> = LazyLock::new(|| {
    Mutex::new(Owners {
        by_inode: HashMap::new(),
        scanned: None,
    })
});

fn scan_owners() -> HashMap<u64, u32> {
    let mut out = HashMap::new();
    for pid in crate::procfs::pids() {
        let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for e in dir.flatten() {
            let Ok(target) = std::fs::read_link(e.path()) else {
                continue;
            };
            let target = target.to_string_lossy();
            if let Some(inode) = target
                .strip_prefix("socket:[")
                .and_then(|s| s.strip_suffix(']'))
                .and_then(|s| s.parse::<u64>().ok())
            {
                out.entry(inode).or_insert(pid);
            }
        }
    }
    out
}

/// The owners of these sockets, rescanning when one is unknown and the
/// last scan is older than `rescan_after`.
pub(crate) fn owners(inodes: &[u64], rescan_after: Duration) -> HashMap<u64, u32> {
    let mut owners = OWNERS.lock().unwrap_or_else(|e| e.into_inner());
    let unknown = inodes
        .iter()
        .any(|i| *i != 0 && !owners.by_inode.contains_key(i));
    if unknown && owners.scanned.is_none_or(|t| t.elapsed() > rescan_after) {
        owners.by_inode = scan_owners();
        owners.scanned = Some(Instant::now());
    }
    inodes
        .iter()
        .filter_map(|i| Some((*i, *owners.by_inode.get(i)?)))
        .collect()
}

/// An address as /proc/net prints it: 32-bit words in hex, each in the
/// machine's byte order.
fn address(hex: &str) -> Option<IpAddr> {
    let words: Vec<u32> = (0..hex.len() / 8)
        .map(|i| u32::from_str_radix(&hex[i * 8..i * 8 + 8], 16).ok())
        .collect::<Option<_>>()?;
    match words.len() {
        1 => Some(IpAddr::V4(Ipv4Addr::from(words[0].to_ne_bytes()))),
        4 => {
            let mut bytes = [0u8; 16];
            for (i, w) in words.iter().enumerate() {
                bytes[i * 4..i * 4 + 4].copy_from_slice(&w.to_ne_bytes());
            }
            Some(ip6(bytes))
        }
        _ => None,
    }
}

fn endpoint(text: &str) -> Option<(IpAddr, u16)> {
    let (ip, port) = text.split_once(':')?;
    Some((address(ip)?, u16::from_str_radix(port, 16).ok()?))
}

/// The kernel's TCP states, numbered as in /proc/net/tcp.
fn tcp_state(code: u8) -> Option<TcpState> {
    Some(match code {
        1 => TcpState::Established,
        2 => TcpState::SynSent,
        3 => TcpState::SynReceived,
        4 => TcpState::FinWait1,
        5 => TcpState::FinWait2,
        6 => TcpState::TimeWait,
        7 => TcpState::Closed,
        8 => TcpState::CloseWait,
        9 => TcpState::LastAck,
        10 => TcpState::Listen,
        11 => TcpState::Closing,
        _ => return None,
    })
}

/// One line of a /proc/net table: local and remote end, state, inode.
type Row = ((IpAddr, u16), (IpAddr, u16), u8, u64);

fn table(file: &str) -> Result<Vec<Row>, String> {
    let text = std::fs::read_to_string(format!("/proc/net/{file}"))
        .map_err(|e| crate::util::error_text(&e))?;
    Ok(text
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_ascii_whitespace().collect();
            Some((
                endpoint(f.get(1)?)?,
                endpoint(f.get(2)?)?,
                u8::from_str_radix(f.get(3)?, 16).ok()?,
                f.get(9)?.parse().ok()?,
            ))
        })
        .collect())
}

pub fn sockets() -> (Vec<Socket>, Option<String>) {
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for (file, proto, what) in [
        ("tcp", Proto::Tcp, "TCP"),
        ("tcp6", Proto::Tcp, "TCP over IPv6"),
        ("udp", Proto::Udp, "UDP"),
        ("udp6", Proto::Udp, "UDP over IPv6"),
    ] {
        match table(file) {
            Ok(list) => rows.extend(list.into_iter().map(|row| (proto, row))),
            // No IPv6 in this kernel: nothing to list, nothing wrong.
            Err(_) if file.ends_with('6') => {}
            Err(e) => errors.push(format!("{what}: {e}")),
        }
    }
    let inodes: Vec<u64> = rows.iter().map(|(_, (_, _, _, inode))| *inode).collect();
    let owners = owners(&inodes, Duration::from_secs(1));
    let all = rows
        .into_iter()
        .map(|(proto, (local, remote, state, inode))| {
            let unspecified = remote.0.is_unspecified() && remote.1 == 0;
            Socket {
                pid: owners.get(&inode).copied().unwrap_or(0),
                proto,
                local: local.0,
                local_port: local.1,
                remote: (!unspecified).then_some(remote),
                // A UDP socket's "state" says only whether it is connected.
                state: (proto == Proto::Tcp).then(|| tcp_state(state)).flatten(),
            }
        })
        .collect();
    let error = (!errors.is_empty()).then(|| errors.join("; "));
    (all, error)
}

/// Every socket in words, by inode: "TCP 192.168.1.5:50312 → 1.2.3.4:443
/// (Open)", "UDP 0.0.0.0:5353", "Unix /run/user/1000/bus". For naming
/// open files (`handles`).
pub(crate) fn names_by_inode() -> HashMap<u64, String> {
    let mut out = HashMap::new();
    let end = |(ip, port): (IpAddr, u16)| match ip {
        IpAddr::V6(v6) => format!("[{v6}]:{port}"),
        v4 => format!("{v4}:{port}"),
    };
    for (file, what) in [
        ("tcp", "TCP"),
        ("tcp6", "TCP"),
        ("udp", "UDP"),
        ("udp6", "UDP"),
    ] {
        for (local, remote, state, inode) in table(file).unwrap_or_default() {
            let tcp = what == "TCP";
            let mut name = format!("{what} {}", end(local));
            if !(remote.0.is_unspecified() && remote.1 == 0) {
                name.push_str(&format!(" \u{2192} {}", end(remote)));
            }
            if let Some(state) = tcp_state(state).filter(|_| tcp) {
                name.push_str(&format!(" ({})", state.label()));
            }
            out.insert(inode, name);
        }
    }
    // Num RefCount Protocol Flags Type St Inode [Path]
    for line in std::fs::read_to_string("/proc/net/unix")
        .unwrap_or_default()
        .lines()
        .skip(1)
    {
        let f: Vec<&str> = line.split_ascii_whitespace().collect();
        let Some(inode) = f.get(6).and_then(|i| i.parse::<u64>().ok()) else {
            continue;
        };
        let name = match f.get(7) {
            Some(path) => format!("Unix {path}"),
            None => "Unix (unnamed)".to_string(),
        };
        out.insert(inode, name);
    }
    out
}

/// Drops one TCP connection of `pid` at once, as the kernel's socket
/// diagnostics interface allows: the program sees it reset. Needs
/// cap_net_admin.
pub fn close_tcp(
    pid: u32,
    remote: Ipv4Addr,
    remote_port: u16,
    local_port: u16,
) -> Result<(), String> {
    let (list, _) = sockets();
    let socket = list
        .iter()
        .find(|s| {
            s.proto == Proto::Tcp
                && s.local_port == local_port
                && s.remote == Some((IpAddr::V4(remote), remote_port))
                && (s.pid == pid || s.pid == 0)
        })
        .ok_or("the connection is gone")?;
    crate::netlink::destroy(
        (socket.local, local_port),
        (IpAddr::V4(remote), remote_port),
    )
    .map_err(|e| match e.raw_os_error() {
        Some(libc::EPERM) | Some(libc::EACCES) => {
            "closing another program's connection needs cap_net_admin".into()
        }
        Some(libc::EOPNOTSUPP) => {
            "this kernel cannot close connections (CONFIG_INET_DIAG_DESTROY)".into()
        }
        Some(libc::ENOENT) => "the connection is gone".into(),
        _ => crate::util::error_text(&e),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_net_addresses() {
        if cfg!(target_endian = "little") {
            assert_eq!(
                endpoint("0100007F:0277"),
                Some((IpAddr::V4(Ipv4Addr::LOCALHOST), 631))
            );
            assert_eq!(
                endpoint("00000000000000000000000001000000:0016").map(|e| e.0),
                Some("::1".parse().unwrap())
            );
        }
    }

    #[test]
    fn finds_own_listening_socket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Another test may have looked for owners a moment ago: the next
        // look waits until a second has passed.
        let started = Instant::now();
        loop {
            let (list, error) = sockets();
            assert!(error.is_none(), "{error:?}");
            let mine = list
                .iter()
                .find(|s| s.local_port == port && s.listening())
                .expect("own listener listed");
            if mine.pid != 0 || started.elapsed() > Duration::from_secs(3) {
                assert_eq!(mine.pid, std::process::id());
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
