//! A key combination that brings Anabolic to the front from any program,
//! through the desktop's GlobalShortcuts portal: on Wayland no program may
//! watch the keyboard itself. The desktop asks the user once (KDE shows a
//! dialog where the key can be changed) and remembers the answer.
//!
//! The portal answers each request later, on a Request object, so a thread
//! of its own holds the session and waits for the presses.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender, bounded};
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MessageType;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{MatchRule, Message};

use super::*;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";

/// The one shortcut Anabolic asks for.
const SHORTCUT: &str = "bring-to-front";

/// A bound combination; dropping it closes the portal session, which gives
/// it back.
pub struct Hotkey {
    stop: Arc<AtomicBool>,
    held: Arc<Mutex<Option<(Connection, OwnedObjectPath)>>>,
    answer: Receiver<Result<(), String>>,
}

/// The portal's name for a key: "CTRL+ALT+Escape", from Windows' modifier
/// flags and virtual key code (what Settings stores).
fn trigger(modifiers: u32, key: u32) -> Option<String> {
    let name = match key {
        0x1B => "Escape".to_string(),
        0x70..=0x87 => format!("F{}", key - 0x6F),
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(key)?.to_string(),
        _ => return None,
    };
    let mut parts = Vec::new();
    if modifiers & MOD_CONTROL != 0 {
        parts.push("CTRL");
    }
    if modifiers & MOD_ALT != 0 {
        parts.push("ALT");
    }
    if modifiers & MOD_SHIFT != 0 {
        parts.push("SHIFT");
    }
    let mut out = parts.join("+");
    if !out.is_empty() {
        out.push('+');
    }
    out.push_str(&name);
    Some(out)
}

/// What a request's answer becomes: (response, results).
type Answer = (u32, HashMap<String, OwnedValue>);

/// Makes a request and waits for its answer. The answer comes as a signal
/// on a Request object whose path follows from our name and a token, so
/// the signal is subscribed to before the request goes out.
fn request(
    conn: &Connection,
    token: &str,
    call: impl FnOnce(&Connection) -> zbus::Result<Message>,
) -> Result<Answer, String> {
    let sender = conn
        .unique_name()
        .ok_or("no name on the bus")?
        .trim_start_matches(':')
        .replace('.', "_");
    let path = format!("{PORTAL_PATH}/request/{sender}/{token}");
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .interface("org.freedesktop.portal.Request")
        .and_then(|r| r.member("Response"))
        .and_then(|r| r.path(path.as_str()))
        .map_err(|e| e.to_string())?
        .build();
    let mut answers =
        MessageIterator::for_match_rule(rule, conn, Some(4)).map_err(|e| e.to_string())?;
    call(conn).map_err(|e| e.to_string())?;
    let answer = answers
        .next()
        .ok_or("the desktop did not answer")?
        .map_err(|e| e.to_string())?;
    answer
        .body()
        .deserialize::<Answer>()
        .map_err(|e| e.to_string())
}

fn refused(response: u32) -> String {
    match response {
        1 => "it was not allowed".into(),
        _ => "the desktop refused it".into(),
    }
}

/// Opens a session, binds the shortcut, and returns the session.
fn bind(conn: &Connection, trigger: &str) -> Result<OwnedObjectPath, String> {
    // Portals of version 1.19 and later know host programs by the id they
    // give; older ones by their systemd scope. Either way it must come first.
    let _ = conn.call_method(
        Some(PORTAL),
        PORTAL_PATH,
        Some("org.freedesktop.host.portal.Registry"),
        "Register",
        &(crate::APP_ID, HashMap::<&str, Value<'_>>::new()),
    );
    // Tokens name the requests; each must be new.
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let token = format!("anabolic_session_{pid}_{n}");
    let (response, results) = request(conn, &token, |conn| {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token.as_str()));
        options.insert(
            "session_handle_token",
            Value::from(format!("anabolic_{pid}_{n}")),
        );
        conn.call_method(
            Some(PORTAL),
            PORTAL_PATH,
            Some(SHORTCUTS),
            "CreateSession",
            &(options,),
        )
    })?;
    if response != 0 {
        return Err(refused(response));
    }
    let session = results
        .get("session_handle")
        .and_then(|v| String::try_from(v.clone()).ok())
        .and_then(|s| OwnedObjectPath::try_from(s).ok())
        .ok_or("the desktop opened no session")?;
    let token = format!("anabolic_bind_{pid}_{n}");
    let bound = request(conn, &token, |conn| {
        let mut shortcut: HashMap<&str, Value<'_>> = HashMap::new();
        shortcut.insert("description", Value::from("Bring Anabolic to the front"));
        shortcut.insert("preferred_trigger", Value::from(trigger));
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token.as_str()));
        conn.call_method(
            Some(PORTAL),
            PORTAL_PATH,
            Some(SHORTCUTS),
            "BindShortcuts",
            &(&session, vec![(SHORTCUT, shortcut)], "", options),
        )
    });
    let fail = |e: String| {
        close(conn, &session);
        e
    };
    let (response, results) = bound.map_err(fail)?;
    if response != 0 {
        return Err(fail(refused(response)));
    }
    // Bound, but perhaps to nothing: KDE gives Ctrl+Alt+Esc to "Kill a
    // window", and leaves ours without a key until the user picks one.
    let key = results
        .get("shortcuts")
        .and_then(|v| Vec::<(String, HashMap<String, OwnedValue>)>::try_from(v.clone()).ok())
        .and_then(|list| {
            list.into_iter()
                .find(|(id, _)| id == SHORTCUT)
                .map(|(_, props)| {
                    props
                        .get("trigger_description")
                        .and_then(|t| String::try_from(t.clone()).ok())
                        .unwrap_or_default()
                })
        });
    match key {
        Some(key) if key.is_empty() => Err(fail(
            "the desktop gave it no key (on KDE, Ctrl+Alt+Esc is \"Kill a window\"): choose one \
             in System Settings, Keyboard, Shortcuts, Anabolic"
                .into(),
        )),
        None => Err(fail("the desktop did not bind it".into())),
        Some(_) => Ok(session),
    }
}

fn close(conn: &Connection, session: &ObjectPath<'_>) {
    let _ = conn.call_method(
        Some(PORTAL),
        session,
        Some("org.freedesktop.portal.Session"),
        "Close",
        &(),
    );
}

/// Waits for presses of the bound shortcut, until the connection closes.
fn serve(conn: &Connection, session: &ObjectPath<'_>, pressed: &dyn Fn()) {
    let Ok(rule) = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .interface(SHORTCUTS)
        .and_then(|r| r.member("Activated"))
        .map(|r| r.build())
    else {
        return;
    };
    let Ok(presses) = MessageIterator::for_match_rule(rule, conn, Some(16)) else {
        return;
    };
    for press in presses.flatten() {
        let body = press.body();
        let Ok((from, id, _, _)) =
            body.deserialize::<(ObjectPath<'_>, &str, u64, HashMap<&str, Value<'_>>)>()
        else {
            continue;
        };
        if from == *session && id == SHORTCUT {
            pressed();
        }
    }
}

impl Hotkey {
    /// Asks the desktop for `modifiers` (MOD_*) with the virtual key `key`;
    /// `pressed` runs on the hotkey's thread each time it is pressed.
    /// Whether the desktop gave it comes later, from `answer`: it may ask the
    /// user first.
    pub fn register(modifiers: u32, key: u32, pressed: impl Fn() + Send + 'static) -> Hotkey {
        let stop = Arc::new(AtomicBool::new(false));
        let held = Arc::new(Mutex::new(None));
        let (tx, answer): (Sender<Result<(), String>>, _) = bounded(1);
        let (stopped, slot) = (stop.clone(), held.clone());
        let tx_spawn = tx.clone();
        let spawned = std::thread::Builder::new()
            .name("anabolic-hotkey".into())
            .spawn(move || {
                let Some(trigger) = trigger(modifiers, key) else {
                    let _ = tx.send(Err("not a key the desktop knows".into()));
                    return;
                };
                let conn = match Connection::session() {
                    Ok(conn) => conn,
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let has_portal = conn
                    .call_method(
                        Some(PORTAL),
                        PORTAL_PATH,
                        Some("org.freedesktop.DBus.Properties"),
                        "Get",
                        &(SHORTCUTS, "version"),
                    )
                    .is_ok();
                if !has_portal {
                    let _ = tx.send(Err(
                        "this desktop offers no global shortcuts (no GlobalShortcuts portal)"
                            .into(),
                    ));
                    return;
                }
                let session = match bind(&conn, &trigger) {
                    Ok(session) => session,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                // Dropped while the desktop was asking: give it back now.
                if stopped.load(Ordering::SeqCst) {
                    close(&conn, &session);
                    return;
                }
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some((conn.clone(), session.clone()));
                }
                let _ = tx.send(Ok(()));
                serve(&conn, &session, &pressed);
            });
        if let Err(e) = spawned {
            let _ = tx_spawn.send(Err(e.to_string()));
        }
        Hotkey { stop, held, answer }
    }

    /// The desktop's answer to the registration, once: None until it has
    /// come.
    pub fn answer(&self) -> Option<Result<(), String>> {
        self.answer.try_recv().ok()
    }
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let held = self.held.lock().ok().and_then(|mut h| h.take());
        if let Some((conn, session)) = held {
            close(&conn, &session);
            // Ends the thread waiting for presses.
            let _ = conn.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_keys_the_way_portals_do() {
        assert_eq!(
            trigger(MOD_CONTROL | MOD_ALT, 0x1B).as_deref(),
            Some("CTRL+ALT+Escape")
        );
        assert_eq!(
            trigger(MOD_CONTROL | MOD_SHIFT, 0x7B).as_deref(),
            Some("CTRL+SHIFT+F12")
        );
        assert_eq!(trigger(MOD_ALT, 0x41).as_deref(), Some("ALT+A"));
        assert_eq!(trigger(0, 0x08), None);
    }
}
