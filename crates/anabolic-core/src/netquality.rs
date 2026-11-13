// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Connection quality, in the manner of PingPlotter and WinMTR: a ping a
//! second to the router, the DNS server, the internet and a game's
//! server, each with its delay, jitter and lost replies over the last
//! minutes, and in words where the trouble lies: between the PC and the
//! router (the Wi-Fi or the cable), past the router (the line or the
//! provider), or only on the way to the server. A route traced hop by hop
//! shows where along the way delay and loss begin. IPv4; nothing but ICMP
//! echo requests is sent.

use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

#[cfg_attr(windows, path = "windows/ping.rs")]
#[cfg_attr(target_os = "linux", path = "linux/ping.rs")]
mod imp;
pub use imp::{dns_servers, gateway};

/// How long a reply may take before it counts as lost.
pub const TIMEOUT: Duration = Duration::from_millis(1000);
/// One ping a second; this many kept (ten minutes).
const KEEP: usize = 600;
const EVERY: Duration = Duration::from_secs(1);

/// What came back for one echo request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reply {
    /// From the address asked, in milliseconds.
    Echo {
        ms: f32,
    },
    /// A router on the way said the time to live ran out: a hop.
    Expired {
        from: Ipv4Addr,
        ms: f32,
    },
    /// A router said the address cannot be reached.
    Unreachable {
        from: Ipv4Addr,
    },
    Lost,
}

/// Sends echo requests; one per thread that pings.
pub use imp::Pinger;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Router,
    Dns,
    Internet,
    Server,
    Other,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Router => "Router",
            Kind::Dns => "DNS server",
            Kind::Internet => "Internet",
            Kind::Server => "Server",
            Kind::Other => "Address",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub kind: Kind,
    pub addr: Ipv4Addr,
    /// What it is, in a few words: "1.1.1.1 (Cloudflare)", the program
    /// whose server it is.
    pub label: String,
}

/// One target's pings, oldest first: the delay, or None for lost.
#[derive(Clone, Debug)]
pub struct Series {
    pub target: Target,
    pub samples: VecDeque<(SystemTime, Option<f32>)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub sent: usize,
    pub lost: usize,
    pub last: Option<f32>,
    pub avg: f32,
    pub min: f32,
    pub max: f32,
    /// The mean change from one reply to the next.
    pub jitter: f32,
}

impl Stats {
    pub fn loss_pct(&self) -> f32 {
        if self.sent == 0 {
            0.0
        } else {
            self.lost as f32 * 100.0 / self.sent as f32
        }
    }

    /// Never answered: a host that does not answer pings.
    pub fn silent(&self) -> bool {
        self.sent >= 5 && self.lost == self.sent
    }
}

impl Series {
    /// Over the last `span` of pings.
    pub fn stats(&self, span: Duration) -> Stats {
        let since = SystemTime::now() - span;
        let window: Vec<Option<f32>> = self
            .samples
            .iter()
            .filter(|(at, _)| *at >= since)
            .map(|(_, ms)| *ms)
            .collect();
        stats(&window)
    }
}

pub fn stats(window: &[Option<f32>]) -> Stats {
    let got: Vec<f32> = window.iter().flatten().copied().collect();
    let mut s = Stats {
        sent: window.len(),
        lost: window.len() - got.len(),
        last: window.last().copied().flatten(),
        ..Stats::default()
    };
    if got.is_empty() {
        return s;
    }
    s.avg = got.iter().sum::<f32>() / got.len() as f32;
    s.min = got.iter().copied().fold(f32::MAX, f32::min);
    s.max = got.iter().copied().fold(0.0, f32::max);
    if got.len() > 1 {
        s.jitter =
            got.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / (got.len() - 1) as f32;
    }
    s
}

/// Pings its targets once a second on threads of its own until dropped.
pub struct Monitor {
    series: Arc<Mutex<Vec<Series>>>,
    stop: Arc<AtomicBool>,
}

impl Monitor {
    pub fn start(targets: Vec<Target>) -> Monitor {
        let series: Arc<Mutex<Vec<Series>>> = Arc::new(Mutex::new(
            targets
                .iter()
                .map(|t| Series {
                    target: t.clone(),
                    samples: VecDeque::new(),
                })
                .collect(),
        ));
        let stop = Arc::new(AtomicBool::new(false));
        for (i, t) in targets.into_iter().enumerate() {
            let (series, stop) = (series.clone(), stop.clone());
            let _ = std::thread::Builder::new()
                .name("ping".into())
                .spawn(move || watch(i, t.addr, &series, &stop));
        }
        Monitor { series, stop }
    }

    pub fn series(&self) -> Vec<Series> {
        self.series.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn targets(&self) -> Vec<Target> {
        self.series().into_iter().map(|s| s.target).collect()
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn watch(i: usize, addr: Ipv4Addr, series: &Mutex<Vec<Series>>, stop: &AtomicBool) {
    let pinger = match Pinger::new() {
        Ok(p) => p,
        Err(_) => return,
    };
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let ms = match pinger.send(addr, 128, TIMEOUT) {
            Reply::Echo { ms } => Some(ms),
            _ => None,
        };
        if let Ok(mut all) = series.lock()
            && let Some(s) = all.get_mut(i)
        {
            s.samples.push_back((SystemTime::now(), ms));
            while s.samples.len() > KEEP {
                s.samples.pop_front();
            }
        }
        if let Some(rest) = EVERY.checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

/// The usual targets: the router, the first DNS server (when it is not
/// the router), and the internet.
pub fn targets() -> Vec<Target> {
    let mut out = Vec::new();
    let router = gateway();
    if let Some(r) = router {
        out.push(Target {
            kind: Kind::Router,
            addr: r,
            label: r.to_string(),
        });
    }
    if let Some(d) = dns_servers()
        .into_iter()
        .find(|d| Some(*d) != router && !d.is_loopback())
    {
        out.push(Target {
            kind: Kind::Dns,
            addr: d,
            label: d.to_string(),
        });
    }
    // Somewhere else than the DNS server, so the two say different things.
    let (addr, label) = if out.iter().any(|t| t.addr == Ipv4Addr::new(1, 1, 1, 1)) {
        (Ipv4Addr::new(8, 8, 8, 8), "8.8.8.8 (Google)")
    } else {
        (Ipv4Addr::new(1, 1, 1, 1), "1.1.1.1 (Cloudflare)")
    };
    out.push(Target {
        kind: Kind::Internet,
        addr,
        label: label.into(),
    });
    out
}

/// An address on the public internet: not private, loopback, link-local
/// or the like. Only such a one can be a game's server.
pub fn public(a: Ipv4Addr) -> bool {
    !(a.is_private()
        || a.is_loopback()
        || a.is_link_local()
        || a.is_broadcast()
        || a.is_multicast()
        || a.is_unspecified()
        || a.is_documentation()
        // Carrier-grade NAT, 100.64.0.0/10.
        || (a.octets()[0] == 100 && (a.octets()[1] & 0xC0) == 64))
}

/// One hop of a traced route.
#[derive(Clone, Debug, PartialEq)]
pub struct Hop {
    pub ttl: u8,
    /// The router that answered; None when none did.
    pub addr: Option<Ipv4Addr>,
    /// The replies' delays, lost ones left out.
    pub ms: Vec<f32>,
    pub sent: usize,
    /// It is the destination.
    pub last: bool,
}

impl Hop {
    pub fn stats(&self) -> Stats {
        let mut window: Vec<Option<f32>> = self.ms.iter().map(|&m| Some(m)).collect();
        window.extend(std::iter::repeat_n(
            None,
            self.sent.saturating_sub(self.ms.len()),
        ));
        stats(&window)
    }
}

/// The route to an address, a hop at a time: `probes` echo requests per
/// hop with a growing time to live, up to 30 hops.
pub fn trace(to: Ipv4Addr, probes: usize) -> Result<Vec<Hop>, String> {
    let pinger = Pinger::new()?;
    let mut hops = Vec::new();
    for ttl in 1..=30u8 {
        let mut hop = Hop {
            ttl,
            addr: None,
            ms: Vec::new(),
            sent: 0,
            last: false,
        };
        for _ in 0..probes {
            hop.sent += 1;
            match pinger.send(to, ttl, TIMEOUT) {
                Reply::Echo { ms } => {
                    hop.addr = Some(to);
                    hop.ms.push(ms);
                    hop.last = true;
                }
                Reply::Expired { from, ms } => {
                    hop.addr.get_or_insert(from);
                    hop.ms.push(ms);
                }
                Reply::Unreachable { from } => {
                    hop.addr.get_or_insert(from);
                    hop.last = true;
                }
                Reply::Lost => {}
            }
        }
        let last = hop.last;
        hops.push(hop);
        if last {
            break;
        }
        // Five silent hops in a row: the rest will not answer either.
        if hops.len() >= 5 && hops.iter().rev().take(5).all(|h| h.addr.is_none()) {
            break;
        }
    }
    Ok(hops)
}

/// Where the trouble lies, in words, from the last minute of each kind.
/// None when there is none worth saying.
pub fn verdict(router: Option<Stats>, internet: Option<Stats>, server: Option<Stats>) -> String {
    let bad = |s: &Stats, ms: f32, jitter: f32| {
        s.sent >= 5 && (s.loss_pct() >= 2.0 || s.avg > ms || s.jitter > jitter)
    };
    if let Some(r) = router
        && bad(&r, 10.0, 8.0)
    {
        return format!(
            "The trouble is between this PC and the router: {}. On Wi-Fi, move closer or use \
another band or channel; on a cable, try another cable or port.",
            describe(&r)
        );
    }
    if let Some(i) = internet
        && !i.silent()
        && bad(&i, 80.0, 20.0)
    {
        return format!(
            "The router answers well; the trouble is past it, on the line or at the provider: \
the internet {}.",
            describe(&i)
        );
    }
    if let Some(s) = server
        && !s.silent()
        && bad(&s, 150.0, 25.0)
    {
        return format!(
            "Only the way to the server is slow ({}): the server itself, or its route. A \
traced route shows from which hop on.",
            describe(&s)
        );
    }
    if router.or(internet).is_some_and(|s| s.sent >= 5) {
        "No trouble: replies come quickly and none are lost.".into()
    } else {
        "Measuring…".into()
    }
}

fn describe(s: &Stats) -> String {
    let mut parts = Vec::new();
    if s.loss_pct() >= 2.0 {
        parts.push(format!("{:.0} % of replies lost", s.loss_pct()));
    }
    parts.push(format!("{:.0} ms on average", s.avg));
    if s.jitter >= 1.0 {
        parts.push(format!("varying by {:.0} ms", s.jitter));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_loss_and_jitter() {
        let s = stats(&[Some(10.0), Some(20.0), None, Some(10.0)]);
        assert_eq!(s.sent, 4);
        assert_eq!(s.lost, 1);
        assert_eq!(s.loss_pct(), 25.0);
        assert_eq!(s.min, 10.0);
        assert_eq!(s.max, 20.0);
        assert_eq!(s.jitter, 10.0);
    }

    #[test]
    fn says_where_the_trouble_is() {
        let good = Stats {
            sent: 60,
            avg: 2.0,
            ..Stats::default()
        };
        let lossy = Stats {
            sent: 60,
            lost: 6,
            avg: 3.0,
            ..Stats::default()
        };
        assert!(verdict(Some(lossy), Some(good), None).contains("between this PC and the router"));
        assert!(verdict(Some(good), Some(lossy), None).contains("past it"));
        assert!(verdict(Some(good), Some(good), None).starts_with("No trouble"));
    }

    #[test]
    fn tells_public_addresses() {
        assert!(public(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!public(Ipv4Addr::new(192, 168, 1, 1)));
        assert!(!public(Ipv4Addr::new(100, 72, 0, 1)));
        assert!(!public(Ipv4Addr::new(127, 0, 0, 1)));
    }
}

#[cfg(test)]
mod live {
    use super::*;

    #[test]
    #[ignore = "pings the internet; run by hand"]
    fn traces_the_route_to_the_internet() {
        for t in targets() {
            let p = Pinger::new().unwrap();
            println!(
                "{:?} {} -> {:?}",
                t.kind,
                t.label,
                p.send(t.addr, 128, TIMEOUT)
            );
        }
        for h in trace(Ipv4Addr::new(1, 1, 1, 1), 2).unwrap() {
            println!(
                "{:>2} {:?} {:?} lost {}",
                h.ttl,
                h.addr,
                h.ms,
                h.sent - h.ms.len()
            );
        }
    }
}
