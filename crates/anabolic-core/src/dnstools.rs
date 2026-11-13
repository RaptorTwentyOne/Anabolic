// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The hosts file, read and written (a copy of the old one kept beside
//! it), and a DNS speed test in the manner of GRC's DNS Benchmark: the
//! DNS servers in use and the well-known public ones, each asked for
//! popular names twice (the second answer comes from its cache) and for
//! names it cannot have cached, timed one query at a time.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Where the hosts file is.
pub fn hosts_path() -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        PathBuf::from(root).join(r"System32\drivers\etc\hosts")
    } else {
        PathBuf::from("/etc/hosts")
    }
}

pub fn read_hosts() -> Result<String, String> {
    let bytes = std::fs::read(hosts_path()).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// One line that maps names to an address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostsEntry {
    pub ip: String,
    pub names: Vec<String>,
    /// Commented out with #: kept, not used.
    pub enabled: bool,
}

pub fn parse_hosts(text: &str) -> Vec<HostsEntry> {
    text.lines()
        .filter_map(|line| {
            let t = line.trim();
            let (enabled, body) = match t.strip_prefix('#') {
                Some(rest) => (false, rest.trim()),
                None => (true, t),
            };
            let body = body.split('#').next().unwrap_or("").trim();
            let mut words = body.split_whitespace();
            let ip = words.next()?;
            ip.parse::<std::net::IpAddr>().ok()?;
            let names: Vec<String> = words.map(str::to_string).collect();
            (!names.is_empty()).then(|| HostsEntry {
                ip: ip.to_string(),
                names,
                enabled,
            })
        })
        .collect()
}

/// Writes the hosts file, keeping the old one as hosts.anabolic-backup,
/// then clears the DNS cache so the change counts at once.
pub fn write_hosts(text: &str) -> Result<String, String> {
    let done = write_at(&hosts_path(), text)?;
    let _ = crate::dnscache::flush();
    Ok(done)
}

fn write_at(path: &std::path::Path, text: &str) -> Result<String, String> {
    let backup = path.with_file_name("hosts.anabolic-backup");
    std::fs::copy(path, &backup).map_err(|e| format!("The old file could not be kept: {e}"))?;
    // Windows' own file ends its lines with CR LF.
    let text = if cfg!(windows) {
        text.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        text.to_string()
    };
    std::fs::write(path, text).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            "Only an administrator may change the hosts file (an antivirus may also refuse)."
                .to_string()
        } else {
            e.to_string()
        }
    })?;
    Ok(format!(
        "The hosts file is saved; the old one is kept as {}.",
        backup.display()
    ))
}

/// A DNS server to time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolver {
    pub addr: Ipv4Addr,
    pub name: String,
    /// One the system uses now.
    pub yours: bool,
}

const PUBLIC: [(&str, [u8; 4]); 7] = [
    ("Cloudflare", [1, 1, 1, 1]),
    ("Google", [8, 8, 8, 8]),
    ("Quad9", [9, 9, 9, 9]),
    ("OpenDNS", [208, 67, 222, 222]),
    ("AdGuard", [94, 140, 14, 14]),
    ("Control D", [76, 76, 2, 0]),
    ("CleanBrowsing", [185, 228, 168, 9]),
];

/// The public servers' second addresses, for their names only.
const SECONDS: [(&str, [u8; 4]); 6] = [
    ("Cloudflare", [1, 0, 0, 1]),
    ("Google", [8, 8, 4, 4]),
    ("Quad9", [149, 112, 112, 112]),
    ("OpenDNS", [208, 67, 220, 220]),
    ("AdGuard", [94, 140, 15, 15]),
    ("Control D", [76, 76, 10, 0]),
];

/// The system's DNS servers, then the public ones.
pub fn resolvers() -> Vec<Resolver> {
    let mut out: Vec<Resolver> = crate::netquality::dns_servers()
        .into_iter()
        .map(|addr| Resolver {
            name: PUBLIC
                .iter()
                .chain(SECONDS.iter())
                .find(|(_, a)| Ipv4Addr::from(*a) == addr)
                .map_or("Yours".to_string(), |(n, _)| n.to_string()),
            addr,
            yours: true,
        })
        .collect();
    for (name, a) in PUBLIC {
        let addr = Ipv4Addr::from(a);
        if !out.iter().any(|r| r.addr == addr) {
            out.push(Resolver {
                addr,
                name: name.into(),
                yours: false,
            });
        }
    }
    out
}

/// Popular names: what most resolvers have cached.
const NAMES: [&str; 12] = [
    "google.com",
    "youtube.com",
    "facebook.com",
    "amazon.com",
    "wikipedia.org",
    "microsoft.com",
    "apple.com",
    "netflix.com",
    "twitch.tv",
    "steampowered.com",
    "discord.com",
    "reddit.com",
];

#[derive(Clone, Debug, PartialEq)]
pub struct Timing {
    pub resolver: Resolver,
    /// The middle answer time for names it had cached, ms.
    pub cached_ms: Option<f32>,
    /// …and for names it had to look up, ms.
    pub uncached_ms: Option<f32>,
    pub sent: usize,
    pub lost: usize,
}

impl Timing {
    /// How it ranks: what a page waits for, mostly cached answers.
    pub fn score(&self) -> f32 {
        match (self.cached_ms, self.uncached_ms) {
            (Some(c), Some(u)) => c * 0.7 + u * 0.3 + self.lost as f32 * 50.0,
            (Some(c), None) => c + 1000.0,
            _ => f32::MAX,
        }
    }
}

/// A query for `name`'s A record; the answer's arrival in ms, or None.
fn query(socket: &UdpSocket, server: Ipv4Addr, name: &str, id: u16) -> Option<f32> {
    let mut packet = Vec::with_capacity(64);
    packet.extend(id.to_be_bytes());
    // Recursion desired; one question.
    packet.extend([0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.split('.') {
        packet.push(label.len() as u8);
        packet.extend(label.as_bytes());
    }
    packet.extend([0, 0, 1, 0, 1]);
    let to = SocketAddr::from((server, 53));
    let started = Instant::now();
    socket.send_to(&packet, to).ok()?;
    let mut buf = [0u8; 1500];
    while let Ok((n, from)) = socket.recv_from(&mut buf) {
        // An answer (QR set) to this question, from that server. An
        // answer that the name does not exist counts: it was answered.
        if from == to && n >= 12 && buf[..2] == id.to_be_bytes() && buf[2] & 0x80 != 0 {
            return Some(started.elapsed().as_secs_f32() * 1000.0);
        }
    }
    None
}

fn middle(mut v: Vec<f32>) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}

fn time(resolver: Resolver) -> Timing {
    let mut t = Timing {
        resolver,
        cached_ms: None,
        uncached_ms: None,
        sent: 0,
        lost: 0,
    };
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return t;
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(1500)));
    let mut id: u16 = (std::process::id() as u16).wrapping_mul(31);
    let mut ask = |name: &str, t: &mut Timing| {
        id = id.wrapping_add(1);
        t.sent += 1;
        let ms = query(&socket, t.resolver.addr, name, id);
        if ms.is_none() {
            t.lost += 1;
        }
        ms
    };
    let mut cached = Vec::new();
    for name in NAMES {
        // The first may have to be looked up; the second is cached.
        let _ = ask(name, &mut t);
        if let Some(ms) = ask(name, &mut t) {
            cached.push(ms);
        }
    }
    let mut uncached = Vec::new();
    for name in NAMES.iter().take(6) {
        // A name no one asked for before: it must ask the domain's own
        // servers.
        let fresh = format!("{}.{name}", crate::random_hex(6));
        if let Some(ms) = ask(&fresh, &mut t) {
            uncached.push(ms);
        }
    }
    t.cached_ms = middle(cached);
    t.uncached_ms = middle(uncached);
    t
}

/// Times every resolver, each on a thread of its own; fastest first.
pub fn benchmark(resolvers: Vec<Resolver>) -> Vec<Timing> {
    let handles: Vec<_> = resolvers
        .into_iter()
        .map(|r| std::thread::spawn(move || time(r)))
        .collect();
    let mut out: Vec<Timing> = handles.into_iter().filter_map(|h| h.join().ok()).collect();
    out.sort_by(|a, b| a.score().total_cmp(&b.score()));
    out
}

/// In words: the fastest, and how the one in use compares.
pub fn verdict(timings: &[Timing]) -> String {
    let Some(best) = timings.first().filter(|t| t.cached_ms.is_some()) else {
        return "No DNS server answered.".into();
    };
    let yours = timings.iter().find(|t| t.resolver.yours);
    let fmt = |t: &Timing| {
        format!(
            "{} ({}) answers in {:.0} ms from its cache, {:.0} ms otherwise",
            t.resolver.name,
            t.resolver.addr,
            t.cached_ms.unwrap_or(0.0),
            t.uncached_ms.unwrap_or(0.0)
        )
    };
    match yours {
        Some(y) if y.resolver.addr == best.resolver.addr => {
            format!("The DNS server in use is the fastest here: {}.", fmt(best))
        }
        Some(y) if y.cached_ms.is_some() => format!(
            "Fastest from here: {}. The one in use, {}, takes {:.0} ms from its cache.",
            fmt(best),
            y.resolver.addr,
            y.cached_ms.unwrap_or(0.0)
        ),
        _ => format!("Fastest from here: {}.", fmt(best)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hosts_lines() {
        let text = "# comment\n127.0.0.1 localhost\n#0.0.0.0 ads.example.com # off\n\
                    ::1 localhost ip6-localhost\nnot-an-ip name\n";
        let e = parse_hosts(text);
        assert_eq!(e.len(), 3);
        assert!(e[0].enabled && e[0].names == ["localhost"]);
        assert!(!e[1].enabled && e[1].names == ["ads.example.com"]);
        assert_eq!(e[2].names, ["localhost", "ip6-localhost"]);
    }

    #[test]
    fn keeps_the_old_file() {
        let dir = std::env::temp_dir().join(format!("anabolic-hosts-{}", crate::random_hex(4)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hosts");
        std::fs::write(&path, "127.0.0.1 localhost\n").unwrap();
        write_at(&path, "127.0.0.1 localhost\n0.0.0.0 ads.example\n").unwrap();
        let kept = std::fs::read_to_string(dir.join("hosts.anabolic-backup")).unwrap();
        assert_eq!(kept, "127.0.0.1 localhost\n");
        let now = std::fs::read_to_string(&path).unwrap();
        assert_eq!(parse_hosts(&now).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ranks_by_cached_answers_mostly() {
        let t = |name: &str, c: f32, u: f32| Timing {
            resolver: Resolver {
                addr: Ipv4Addr::LOCALHOST,
                name: name.into(),
                yours: false,
            },
            cached_ms: Some(c),
            uncached_ms: Some(u),
            sent: 30,
            lost: 0,
        };
        assert!(t("a", 10.0, 60.0).score() < t("b", 30.0, 40.0).score());
    }

    #[test]
    #[ignore = "asks the internet's DNS servers; run by hand"]
    fn times_the_resolvers() {
        let r = benchmark(resolvers());
        for t in &r {
            println!(
                "{:<14} {:<16} cached {:?} uncached {:?} lost {}/{}{}",
                t.resolver.name,
                t.resolver.addr,
                t.cached_ms,
                t.uncached_ms,
                t.lost,
                t.sent,
                if t.resolver.yours { "  (yours)" } else { "" }
            );
        }
        println!("{}", verdict(&r));
    }
}
