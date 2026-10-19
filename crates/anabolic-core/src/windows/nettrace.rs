//! Per-process and per-connection network traffic from the
//! Microsoft-Windows-Kernel-Network ETW provider (needs admin), and every
//! name a program looks up, from Microsoft-Windows-DNS-Client: the DNS
//! client writes the event in the asking program itself, so it says who
//! asked, what for, and what came back.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};

use windows::Win32::System::Diagnostics::Etw::EVENT_RECORD;
use windows::core::GUID;

use crate::etw::{self, Session};

use super::*;

/// A kernel trace session: administrators only.
pub const NEEDS_ELEVATION: bool = true;

const KERNEL_NETWORK: GUID = GUID::from_u128(0x7dd42a49_5329_4832_8dfd_43d979153a88);
const KEYWORD_IPV4: u64 = 0x10;
const KEYWORD_IPV6: u64 = 0x20;
const LEVEL_INFORMATION: u8 = 4;
const DNS_CLIENT: GUID = GUID::from_u128(0x1c95126e_7eea_49a9_a3fe_a378b03ddb4d);
/// Microsoft-Windows-Kernel-Process: every process start and stop, with the
/// parent, even for one that lives a few milliseconds.
const KERNEL_PROCESS: GUID = GUID::from_u128(0x22fb2cd6_0e7b_422b_a0c7_2fad1fd0e716);
const KEYWORD_PROCESS: u64 = 0x10;
const PROCESS_START: u16 = 1;
const PROCESS_STOP: u16 = 2;
/// "DNS query is completed", in the program that asked.
const DNS_QUERY_COMPLETED: u16 = 3008;
const LEVEL_VERBOSE: u8 = 5;

/// ProcessStart, version 4 (Windows 10 and 11; older layouts are left to
/// the process scans): ProcessID, ProcessSequenceNumber, CreateTime,
/// ParentProcessID, ParentProcessSequenceNumber, SessionID, Flags,
/// ProcessTokenElevationType, ProcessTokenIsElevated, MandatoryLabel (a
/// SID), ImageName, ...
pub fn parse_start(version: u8, d: &[u8]) -> Option<ProcessStart> {
    if version < 4 {
        return None;
    }
    let pid = u32_le(d, 0)?;
    let create_time = i64::from_le_bytes(d.get(12..20)?.try_into().ok()?);
    let ppid = u32_le(d, 20)?;
    let elevated = u32_le(d, 44)? != 0;
    // The SID: revision, sub-authority count, 6-byte authority, then the
    // sub-authorities.
    let count = *d.get(49)? as usize;
    let (image, _) = wide_str(d, 48 + 8 + 4 * count)?;
    Some(ProcessStart {
        pid,
        create_time,
        ppid,
        elevated,
        image,
    })
}

/// ProcessStop, version 2: ProcessID, ProcessSequenceNumber, CreateTime,
/// ExitTime, ExitCode, ...
pub fn parse_stop(version: u8, d: &[u8]) -> Option<ProcessStop> {
    if version < 2 {
        return None;
    }
    Some(ProcessStop {
        pid: u32_le(d, 0)?,
        create_time: i64::from_le_bytes(d.get(12..20)?.try_into().ok()?),
        exit_time: i64::from_le_bytes(d.get(20..28)?.try_into().ok()?),
        exit_code: u32_le(d, 28)?,
    })
}

fn wide_str(d: &[u8], at: usize) -> Option<(String, usize)> {
    let mut chars = Vec::new();
    let mut i = at;
    loop {
        let c = u16::from_le_bytes(d.get(i..i + 2)?.try_into().ok()?);
        i += 2;
        if c == 0 {
            return Some((String::from_utf16_lossy(&chars), i));
        }
        chars.push(c);
    }
}

/// QueryName, QueryType (u32), QueryOptions (u64), QueryStatus (u32),
/// QueryResults: "type: 5 alias.example;1.2.3.4;::ffff:5.6.7.8;".
pub fn parse_lookup(pid: u32, d: &[u8]) -> Option<DnsLookup> {
    let (name, at) = wide_str(d, 0)?;
    let kind = u32_le(d, at)? as u16;
    let status = u32_le(d, at + 12)?;
    let (results, _) = wide_str(d, at + 16).unwrap_or_default();
    // 87: the A half of a lookup the AAAA half answers for both.
    if status == 87 || name.is_empty() {
        return None;
    }
    let mut addresses = Vec::new();
    let mut aliases = Vec::new();
    for part in results.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        if let Some(rest) = part.strip_prefix("type:") {
            // "type: 5 alias": a CNAME on the way.
            if let Some(alias) = rest.trim().strip_prefix("5 ") {
                aliases.push(Arc::from(alias.trim()));
            }
        } else if let Ok(ip) = part.parse::<IpAddr>() {
            let ip = match ip {
                IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
                v4 => v4,
            };
            if !addresses.contains(&ip) {
                addresses.push(ip);
            }
        }
    }
    Some(DnsLookup {
        at: std::time::SystemTime::now(),
        pid,
        name: name.into(),
        kind,
        status,
        addresses,
        aliases,
    })
}

pub struct NetTracer {
    traffic: Arc<Mutex<Traffic>>,
    _session: Session,
}

impl NetTracer {
    pub fn start() -> Result<Self, String> {
        let session = Session::start("Anabolic-Network", 0, 0)?;
        session.enable(
            &KERNEL_NETWORK,
            LEVEL_INFORMATION,
            KEYWORD_IPV4 | KEYWORD_IPV6,
        )?;
        // Names are a bonus: traffic still counts if this is refused.
        let _ = session.enable_ids(&DNS_CLIENT, LEVEL_VERBOSE, u64::MAX, &[DNS_QUERY_COMPLETED]);
        // Lineage, likewise.
        let _ = session.enable_ids(
            &KERNEL_PROCESS,
            LEVEL_VERBOSE,
            KEYWORD_PROCESS,
            &[PROCESS_START, PROCESS_STOP],
        );
        let traffic = Arc::new(Mutex::new(Traffic::default()));
        let sink = traffic.clone();
        session.consume(move |record| {
            if record.EventHeader.ProviderId == KERNEL_PROCESS {
                let descriptor = record.EventHeader.EventDescriptor;
                let d = etw::user_data(record);
                if let Ok(mut t) = sink.lock() {
                    match descriptor.Id {
                        PROCESS_START => t.starts.extend(parse_start(descriptor.Version, d)),
                        PROCESS_STOP => t.stops.extend(parse_stop(descriptor.Version, d)),
                        _ => {}
                    }
                }
                return;
            }
            if record.EventHeader.ProviderId == DNS_CLIENT {
                if record.EventHeader.EventDescriptor.Id == DNS_QUERY_COMPLETED
                    && let Some(lookup) =
                        parse_lookup(record.EventHeader.ProcessId, etw::user_data(record))
                    && let Ok(mut t) = sink.lock()
                {
                    t.lookups.push(lookup);
                }
                return;
            }
            if let Some((key, received, bytes)) = parse(record)
                && let Ok(mut t) = sink.lock()
            {
                let bytes = bytes as u64;
                let process = t.per_pid.entry(key.pid).or_default();
                let conn = if received {
                    &mut process.0
                } else {
                    &mut process.1
                };
                *conn += bytes;
                let conn = t.per_conn.entry(key).or_default();
                if received {
                    conn.0 += bytes;
                } else {
                    conn.1 += bytes;
                }
            }
        })?;
        Ok(Self {
            traffic,
            _session: session,
        })
    }

    /// Everything counted since the previous call.
    pub fn take(&self) -> Traffic {
        self.traffic
            .lock()
            .map(|mut t| std::mem::take(&mut *t))
            .unwrap_or_default()
    }
}

fn u16_be(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// Send/receive events all start with PID and size, then the destination
/// and source addresses and ports (ports in network byte order). The
/// destination is the remote end, also for receives.
fn parse(record: &EVENT_RECORD) -> Option<(ConnKey, bool, u32)> {
    let (received, v6, proto) = match record.EventHeader.EventDescriptor.Id {
        10 => (false, false, Proto::Tcp),
        11 => (true, false, Proto::Tcp),
        26 => (false, true, Proto::Tcp),
        27 => (true, true, Proto::Tcp),
        42 => (false, false, Proto::Udp),
        43 => (true, false, Proto::Udp),
        58 => (false, true, Proto::Udp),
        59 => (true, true, Proto::Udp),
        _ => return None,
    };
    let d = etw::user_data(record);
    let pid = u32_le(d, 0)?;
    let size = u32_le(d, 4)?;
    let (remote, remote_port, local_port) = if v6 {
        let addr: [u8; 16] = d.get(8..24)?.try_into().ok()?;
        (crate::sockets::ip6(addr), u16_be(d, 40)?, u16_be(d, 42)?)
    } else {
        let addr: [u8; 4] = d.get(8..12)?.try_into().ok()?;
        (
            IpAddr::V4(Ipv4Addr::from(addr)),
            u16_be(d, 16)?,
            u16_be(d, 18)?,
        )
    };
    Some((
        ConnKey {
            pid,
            proto,
            remote,
            remote_port,
            local_port,
        },
        received,
        size,
    ))
}

#[cfg(test)]
mod tests {

    fn wide(text: &str) -> Vec<u8> {
        text.encode_utf16()
            .chain([0])
            .flat_map(|c| c.to_le_bytes())
            .collect()
    }

    #[test]
    fn reads_a_completed_lookup() {
        let mut d = wide("www.microsoft.com");
        d.extend(28u32.to_le_bytes());
        d.extend(0u64.to_le_bytes());
        d.extend(0u32.to_le_bytes());
        d.extend(wide(
            "type: 5 www.microsoft.com-c-3.edgekey.net;type: 5 e13678.dscb.akamaiedge.net;::ffff:184.30.158.70;",
        ));
        let l = parse_lookup(42, &d).expect("a lookup");
        assert_eq!(&*l.name, "www.microsoft.com");
        assert_eq!(l.kind_label(), "AAAA");
        assert_eq!(l.addresses, ["184.30.158.70".parse::<IpAddr>().unwrap()]);
        assert_eq!(l.aliases.len(), 2);
        assert_eq!(l.status_label(), "answered");
        // The A half of a lookup the AAAA half answers is skipped.
        let mut half = wide("example.org");
        half.extend(1u32.to_le_bytes());
        half.extend(0u64.to_le_bytes());
        half.extend(87u32.to_le_bytes());
        half.extend(wide(""));
        assert!(parse_lookup(42, &half).is_none());
    }

    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    /// Needs an elevated test run; skips itself otherwise.
    #[test]
    fn sees_own_tcp_traffic() {
        let _turn = crate::etw::kernel_turn();
        if !crate::hardening::is_elevated() {
            eprintln!("not elevated, skipping");
            return;
        }
        let tracer = NetTracer::start().expect("could not start the trace");
        std::thread::sleep(Duration::from_millis(500));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 1 << 20];
            let mut total = 0;
            while total < buf.len() {
                let n = s.read(&mut buf[total..]).unwrap();
                if n == 0 {
                    break;
                }
                total += n;
            }
        });
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client.write_all(&vec![7u8; 1 << 20]).unwrap();
        drop(client);
        server.join().unwrap();
        // Events arrive with the session's flush; a loaded machine is slower.
        let me = std::process::id();
        let (mut rx, mut tx, mut seen) = (0, 0, false);
        let until = std::time::Instant::now() + Duration::from_secs(8);
        while tx < 1 << 20 && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(500));
            let traffic = tracer.take();
            let (r, t) = traffic.per_pid.get(&me).copied().unwrap_or_default();
            rx += r;
            tx += t;
            seen |= traffic
                .per_conn
                .keys()
                .any(|k| k.pid == me && k.remote_port == port);
        }
        assert!(tx >= 1 << 20, "sent {tx}, received {rx}");
        assert!(seen, "the connection was not seen");
    }
}
