//! One Anabolic per user and display, through an abstract Unix socket: the
//! first instance listens on it, a later one connects (which asks the
//! first to come forward) and exits. The kernel removes the name when the
//! first instance's socket closes, so a crash leaves nothing behind.

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;
use std::time::Duration;

use super::*;

/// The listening socket the first instance serves activations on.
pub type Activation = UnixListener;

/// One name per user and display: two people signed in, or one person on
/// two seats, each have their own Anabolic.
fn socket_name() -> String {
    let uid = unsafe { libc::getuid() };
    let display = std::env::var("WAYLAND_DISPLAY")
        .or_else(|_| std::env::var("DISPLAY"))
        .unwrap_or_default();
    format!("anabolic-{uid}-{display}")
}

/// Becomes the session's Anabolic, or hands over to the one already running.
pub fn claim() -> Claim {
    claim_named(&socket_name())
}

fn claim_named(name: &str) -> Claim {
    use std::os::linux::net::SocketAddrExt;
    let Ok(addr) = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()) else {
        return Claim::Unavailable;
    };
    match UnixListener::bind_addr(&addr) {
        Ok(listener) => Claim::First(listener),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            match UnixStream::connect_addr(&addr) {
                Ok(mut stream) => {
                    let _ = stream.write_all(b"activate\n");
                    Claim::Running
                }
                // Taken, but nobody answers: run on our own.
                Err(_) => Claim::Unavailable,
            }
        }
        Err(_) => Claim::Unavailable,
    }
}

/// Waits up to `timeout` for process `pid` to exit: the instance that is
/// closing to make room for this one.
pub fn await_exit(pid: u32, timeout: Duration) {
    let key = crate::model::ProcKey {
        pid,
        create_time: 0,
    };
    if let Ok(process) = crate::util::open_process(key) {
        process.wait_ended(timeout.as_millis().min(i32::MAX as u128) as i32);
    }
}

/// Runs `on_activate` on a thread of its own every time another instance
/// asks for this one.
pub fn serve(listener: Activation, on_activate: impl Fn() + Send + 'static) {
    let spawned = thread::Builder::new()
        .name("anabolic-activate".into())
        .spawn(move || {
            for stream in listener.incoming() {
                if stream.is_ok() {
                    on_activate();
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("anabolic: no activation thread: {e}");
    }
}

/// The window is raised by the UI itself (a Wayland compositor decides who
/// gets the focus, not the program asking): nothing to do here.
pub fn bring_to_front() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn second_claim_signals_the_first() {
        let name = format!("anabolic-test-{}", std::process::id());
        let Claim::First(listener) = claim_named(&name) else {
            panic!("the first claim should own the socket");
        };
        let (tx, rx) = mpsc::channel();
        serve(listener, move || {
            let _ = tx.send(());
        });
        assert!(matches!(claim_named(&name), Claim::Running));
        rx.recv_timeout(Duration::from_secs(2))
            .expect("the first instance was not signalled");
        assert!(matches!(claim_named(&name), Claim::Running));
        rx.recv_timeout(Duration::from_secs(2))
            .expect("the second signal was lost");
    }
}
