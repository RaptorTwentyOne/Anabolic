//! Per-process and per-connection network traffic from the
//! Microsoft-Windows-Kernel-Network ETW provider (needs admin).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};

use windows::Win32::System::Diagnostics::Etw::EVENT_RECORD;
use windows::core::GUID;

use crate::etw::{self, Session};

const KERNEL_NETWORK: GUID = GUID::from_u128(0x7dd42a49_5329_4832_8dfd_43d979153a88);
const KEYWORD_IPV4: u64 = 0x10;
const KEYWORD_IPV6: u64 = 0x20;
const LEVEL_INFORMATION: u8 = 4;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Proto {
    Tcp,
    Udp,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ConnKey {
    pub pid: u32,
    pub proto: Proto,
    pub remote: IpAddr,
    pub remote_port: u16,
    pub local_port: u16,
}

/// Bytes seen since the last `take`.
#[derive(Default, Debug)]
pub struct Traffic {
    /// pid -> (received, sent)
    pub per_pid: HashMap<u32, (u64, u64)>,
    pub per_conn: HashMap<ConnKey, (u64, u64)>,
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
        let traffic = Arc::new(Mutex::new(Traffic::default()));
        let sink = traffic.clone();
        session.consume(move |record| {
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
        (
            IpAddr::V6(Ipv6Addr::from(addr)),
            u16_be(d, 40)?,
            u16_be(d, 42)?,
        )
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
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    /// Needs an elevated test run; skips itself otherwise.
    #[test]
    fn sees_own_tcp_traffic() {
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
