//! Network traffic on Linux: TCP's own byte counters per connection, read
//! from the kernel's socket diagnostics on every refresh and matched to
//! the process holding each socket; process starts and stops from the
//! process events connector, when Anabolic may listen to it.
//!
//! What this does not see: UDP (the kernel keeps no byte counts for its
//! sockets), the last moments of a connection closed between two refreshes,
//! and the names programs look up (Linux has no system-wide record of
//! that: each program asks its own resolver library).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use crate::netlink::{self, DiagSocket, ProcEvent, ProcEvents};
use crate::procfs;

use super::*;

/// Reading the socket tables needs no right: the collector runs it for
/// everyone.
pub const NEEDS_ELEVATION: bool = false;

/// How old the socket-to-process map may be before a new connection makes
/// it read again: a new connection's bytes wait for their owner meanwhile.
const RESCAN: Duration = Duration::from_secs(2);

/// The TCP state a listening socket has.
const LISTEN: u8 = 10;

#[derive(Clone, Copy)]
struct Seen {
    received: u64,
    sent: u64,
}

#[derive(Default)]
struct State {
    /// By socket cookie: the counters at the last look.
    seen: HashMap<u64, Seen>,
    /// The first look only sets the counters: what came before was not
    /// seen happening.
    primed: bool,
    /// Processes running or started since, with their start time: a stop
    /// event names only the PID.
    started: HashMap<u32, i64>,
}

pub struct NetTracer {
    state: Mutex<State>,
    events: Option<ProcEvents>,
    /// Every user's sockets can be matched to their process (root, or
    /// cap_sys_ptrace); else only this user's.
    all_users: bool,
}

impl NetTracer {
    pub fn start() -> Result<Self, String> {
        netlink::dump(Proto::Tcp).map_err(|e| {
            format!(
                "the kernel's socket diagnostics could not be read: {}",
                crate::util::error_text(&e)
            )
        })?;
        let events = ProcEvents::open().ok();
        let mut state = State::default();
        if events.is_some() {
            for pid in procfs::pids() {
                if let Some(stat) = procfs::stat(pid) {
                    state.started.insert(pid, stat.create_time());
                }
            }
        }
        let tracer = Self {
            state: Mutex::new(state),
            events,
            all_users: unsafe { libc::geteuid() } == 0 || crate::hardening::has_capability(19),
        };
        // The counters as they are now: only what follows is traffic.
        tracer.take();
        Ok(tracer)
    }

    /// Everything counted since the previous call.
    pub fn take(&self) -> Traffic {
        let mut traffic = Traffic::default();
        let Ok(mut state) = self.state.lock() else {
            return traffic;
        };
        self.count(&mut state, &mut traffic);
        self.processes(&mut state, &mut traffic);
        traffic
    }

    fn count(&self, state: &mut State, traffic: &mut Traffic) {
        let sockets = netlink::dump(Proto::Tcp).unwrap_or_default();
        let uid = unsafe { libc::getuid() };
        let wanted: Vec<&DiagSocket> = sockets
            .iter()
            .filter(|s| {
                s.inode != 0
                    && s.state != LISTEN
                    && s.received.is_some()
                    && (self.all_users || s.uid == uid)
            })
            .collect();
        let inodes: Vec<u64> = wanted.iter().map(|s| s.inode).collect();
        let owners = crate::sockets::owners(&inodes, RESCAN);
        let mut next = HashMap::with_capacity(wanted.len());
        for s in wanted {
            let now = Seen {
                received: s.received.unwrap_or(0),
                sent: s.sent.unwrap_or(0),
            };
            let before = state.seen.get(&s.cookie).copied();
            let Some(&pid) = owners.get(&s.inode) else {
                // Whose it is is not known yet: its bytes wait for the next
                // look at the processes' sockets.
                match before {
                    Some(seen) => next.insert(s.cookie, seen),
                    None if !state.primed => next.insert(s.cookie, now),
                    None => None,
                };
                continue;
            };
            // A connection new since the last look: all of it is new.
            let before = before.unwrap_or(if state.primed {
                Seen {
                    received: 0,
                    sent: 0,
                }
            } else {
                now
            });
            next.insert(s.cookie, now);
            let rx = now.received.saturating_sub(before.received);
            let tx = now.sent.saturating_sub(before.sent);
            if rx == 0 && tx == 0 {
                continue;
            }
            let process = traffic.per_pid.entry(pid).or_default();
            process.0 += rx;
            process.1 += tx;
            let conn = traffic
                .per_conn
                .entry(ConnKey {
                    pid,
                    proto: Proto::Tcp,
                    remote: s.remote.0,
                    remote_port: s.remote.1,
                    local_port: s.local.1,
                })
                .or_default();
            conn.0 += rx;
            conn.1 += tx;
        }
        state.seen = next;
        state.primed = true;
    }

    /// Starts and stops from the connector: programs that ran for less
    /// than a refresh are known to have been, and who started them.
    fn processes(&self, state: &mut State, traffic: &mut Traffic) {
        let Some(events) = &self.events else {
            return;
        };
        let mut seen = Vec::new();
        events.drain(&mut seen);
        for event in seen {
            match event {
                // A new process (not a thread) that runs its parent's
                // program until it starts its own.
                ProcEvent::Fork { pid, tid, .. } if pid == tid => {
                    if let Some(stat) = procfs::stat(pid) {
                        state.started.insert(pid, stat.create_time());
                    }
                }
                ProcEvent::Exec { pid } => {
                    let Some(stat) = procfs::stat(pid) else {
                        continue;
                    };
                    state.started.insert(pid, stat.create_time());
                    let status =
                        std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
                    let elevated = procfs::status_value(&status, "Uid")
                        .and_then(|ids| ids.split_ascii_whitespace().nth(1))
                        .is_some_and(|euid| euid == "0");
                    traffic.starts.push(ProcessStart {
                        pid,
                        create_time: stat.create_time(),
                        ppid: stat.ppid,
                        elevated,
                        image: procfs::exe(pid).unwrap_or(stat.comm),
                    });
                }
                ProcEvent::Exit { pid, tid, status } if pid == tid => {
                    if let Some(create_time) = state.started.remove(&pid) {
                        traffic.stops.push(ProcessStop {
                            pid,
                            create_time,
                            exit_time: crate::sysinfo::now_filetime(),
                            // The wait status as the kernel gives it: the
                            // signal that ended it in the low bits, else
                            // the exit code above them.
                            exit_code: status,
                        });
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn counts_a_connection_of_this_process() {
        let tracer = NetTracer::start().expect("socket diagnostics");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        client.write_all(&[1u8; 50_000]).unwrap();
        let mut got = vec![0u8; 50_000];
        server.read_exact(&mut got).unwrap();
        let me = std::process::id();
        let key = ConnKey {
            pid: me,
            proto: Proto::Tcp,
            remote: "127.0.0.1".parse().unwrap(),
            remote_port: port,
            local_port: client.local_addr().unwrap().port(),
        };
        // The new connection's owner is learnt at the next look at the
        // processes' sockets; its bytes wait for it.
        let started = std::time::Instant::now();
        let (mut rx, mut tx, mut conn) = (0, 0, 0);
        while conn == 0 && started.elapsed() < RESCAN * 2 {
            let traffic = tracer.take();
            let (r, t) = traffic.per_pid.get(&me).copied().unwrap_or_default();
            rx += r;
            tx += t;
            conn += traffic.per_conn.get(&key).map_or(0, |c| c.1);
            std::thread::sleep(Duration::from_millis(200));
        }
        // Both ends are ours: each counts what it sent and received.
        assert!(tx >= 50_000 && rx >= 50_000, "rx {rx}, tx {tx}");
        assert!(conn >= 50_000);
        // Counted once: nothing new since.
        assert!(!tracer.take().per_conn.contains_key(&key));
    }
}
