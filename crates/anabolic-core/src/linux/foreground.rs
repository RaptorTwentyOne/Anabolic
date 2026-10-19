//! The program in front. A Wayland compositor tells programs nothing of
//! each other's windows, so it is asked where it can be:
//!
//! - KWin (KDE, Wayland or X11): a small script of Anabolic's, loaded into
//!   KWin over D-Bus, reports the active window's process and whether it
//!   fills its monitor, each time that changes. Nothing is polled.
//! - Other X11 desktops: the window manager's _NET_ACTIVE_WINDOW.
//! - GNOME and others on Wayland: unknown.
//!
//! The script is unloaded when Anabolic exits; one left by an instance
//! that crashed is replaced at the next start.

use std::sync::{Mutex, OnceLock};

use zbus::blocking::Connection;

use super::*;

const KWIN: &str = "org.kde.KWin";
/// The name the script is loaded under: one per user, so a leftover is
/// found and replaced.
const PLUGIN: &str = "anabolic-foreground";
const PATH: &str = "/io/github/RaptorTwentyOne/Anabolic/Foreground";
const INTERFACE: &str = "io.github.RaptorTwentyOne.Anabolic.Foreground";

/// What the script last said.
static FRONT: Mutex<Option<Front>> = Mutex::new(None);

enum Source {
    KWin,
    X11(Box<X11>),
}

static SOURCE: OnceLock<Option<Source>> = OnceLock::new();

/// The KWin connection, kept for unloading the script at exit.
static KWIN_CONN: OnceLock<Connection> = OnceLock::new();

pub fn front() -> Option<Front> {
    match SOURCE.get_or_init(start).as_ref()? {
        Source::KWin => *FRONT.lock().ok()?,
        Source::X11(x11) => x11.front(),
    }
}

fn start() -> Option<Source> {
    if let Some(()) = start_kwin() {
        return Some(Source::KWin);
    }
    let wayland = std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland")
        || std::env::var_os("WAYLAND_DISPLAY").is_some();
    if wayland {
        return None;
    }
    X11::connect().map(|x11| Source::X11(Box::new(x11)))
}

/// Where the script calls back to: "<pid> <1 if it fills its monitor>".
struct Callback;

#[zbus::interface(name = "io.github.RaptorTwentyOne.Anabolic.Foreground")]
impl Callback {
    fn front(&self, state: &str) {
        let mut parts = state.split_ascii_whitespace();
        let pid = parts
            .next()
            .and_then(|p| p.parse::<u32>().ok())
            .unwrap_or(0);
        let fullscreen = parts.next() == Some("1");
        if let Ok(mut front) = FRONT.lock() {
            *front = (pid != 0).then_some(Front { pid, fullscreen });
        }
    }
}

/// The script: it follows the active window, and calls back when it, or
/// whether it fills its monitor, changes. Desktop and panel windows (the
/// shell itself) count as nothing in front.
fn script(service: &str) -> String {
    format!(
        r#"// Anabolic: tells it which window is in front.
function covers(w) {{
    if (w.fullScreen) return true;
    const area = workspace.clientArea(KWin.FullScreenArea, w);
    const g = w.frameGeometry;
    return g.x <= area.x && g.y <= area.y
        && g.x + g.width >= area.x + area.width
        && g.y + g.height >= area.y + area.height;
}}
let watched = null;
let said = "";
function report() {{
    const w = workspace.activeWindow;
    const state = (w && !w.desktopWindow && !w.dock)
        ? w.pid + " " + (covers(w) ? 1 : 0)
        : "0 0";
    if (state == said) return;
    said = state;
    callDBus("{service}", "{PATH}", "{INTERFACE}", "Front", state);
}}
function follow(w) {{
    if (watched) {{
        watched.fullScreenChanged.disconnect(report);
        watched.frameGeometryChanged.disconnect(report);
    }}
    watched = w;
    if (w) {{
        w.fullScreenChanged.connect(report);
        w.frameGeometryChanged.connect(report);
    }}
    report();
}}
workspace.windowActivated.connect(follow);
follow(workspace.activeWindow);
"#
    )
}

fn script_file() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(std::path::Path::new(&dir).join(format!("{PLUGIN}.js")))
}

fn scripting<B>(conn: &Connection, method: &str, body: &B) -> zbus::Result<zbus::Message>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    conn.call_method(
        Some(KWIN),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        method,
        body,
    )
}

/// Loads the script into KWin, when there is a KWin.
fn start_kwin() -> Option<()> {
    let conn = Connection::session().ok()?;
    let kwin = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "NameHasOwner",
            &KWIN,
        )
        .ok()?
        .body()
        .deserialize::<bool>()
        .ok()?;
    if !kwin {
        return None;
    }
    conn.object_server().at(PATH, Callback).ok()?;
    let service = conn.unique_name()?.to_string();
    let file = script_file()?;
    std::fs::write(&file, script(&service)).ok()?;
    // A leftover from an instance that did not get to unload it.
    let _ = scripting(&conn, "unloadScript", &PLUGIN);
    let id: i32 = scripting(
        &conn,
        "loadScript",
        &(file.to_string_lossy().as_ref(), PLUGIN),
    )
    .ok()?
    .body()
    .deserialize()
    .ok()?;
    if id < 0 {
        return None;
    }
    let run = conn.call_method(
        Some(KWIN),
        format!("/Scripting/Script{id}").as_str(),
        Some("org.kde.kwin.Script"),
        "run",
        &(),
    );
    if run.is_err() {
        let _ = scripting(&conn, "start", &());
    }
    let _ = KWIN_CONN.set(conn);
    unsafe { libc::atexit(unload) };
    Some(())
}

/// At exit: KWin keeps a script until it is unloaded.
extern "C" fn unload() {
    if let Some(conn) = KWIN_CONN.get() {
        let _ = scripting(conn, "unloadScript", &PLUGIN);
    }
    if let Some(file) = script_file() {
        let _ = std::fs::remove_file(file);
    }
}

/// The X11 window manager's idea of the active window.
struct X11 {
    conn: x11rb::rust_connection::RustConnection,
    root: u32,
    active: u32,
    pid: u32,
    state: u32,
    fullscreen: u32,
}

impl X11 {
    fn connect() -> Option<X11> {
        use x11rb::protocol::xproto::ConnectionExt;
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = x11rb::connection::Connection::setup(&conn)
            .roots
            .get(screen)?
            .root;
        let atom = |name: &str| -> Option<u32> {
            Some(
                conn.intern_atom(false, name.as_bytes())
                    .ok()?
                    .reply()
                    .ok()?
                    .atom,
            )
        };
        Some(X11 {
            active: atom("_NET_ACTIVE_WINDOW")?,
            pid: atom("_NET_WM_PID")?,
            state: atom("_NET_WM_STATE")?,
            fullscreen: atom("_NET_WM_STATE_FULLSCREEN")?,
            root,
            conn,
        })
    }

    /// One 32-bit list property of a window.
    fn property(
        &self,
        window: u32,
        name: u32,
        kind: x11rb::protocol::xproto::AtomEnum,
    ) -> Vec<u32> {
        use x11rb::protocol::xproto::ConnectionExt;
        self.conn
            .get_property(false, window, name, kind, 0, 64)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().map(|v| v.collect()))
            .unwrap_or_default()
    }

    fn front(&self) -> Option<Front> {
        use x11rb::protocol::xproto::AtomEnum;
        let window = *self
            .property(self.root, self.active, AtomEnum::WINDOW)
            .first()?;
        if window == 0 {
            return None;
        }
        let pid = *self
            .property(window, self.pid, AtomEnum::CARDINAL)
            .first()?;
        let fullscreen = self
            .property(window, self.state, AtomEnum::ATOM)
            .contains(&self.fullscreen);
        Some(Front { pid, fullscreen })
    }
}
