//! The notification area icon on Linux: a StatusNotifierItem on the session
//! bus (what KDE's system tray shows, and GNOME's with the AppIndicator
//! extension), its menu over com.canonical.dbusmenu, and notifications
//! through org.freedesktop.Notifications. The bus answers on a thread of
//! zbus's own, so clicks work while the UI thread sleeps.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use eframe::egui;
use zbus::blocking::Connection;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{Type, Value};

pub enum TrayEvent {
    /// The window was brought back.
    Open,
    Quit,
}

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

/// The menu's entries; 0 is the root.
const OPEN: i32 = 1;
const SEPARATOR: i32 = 2;
const QUIT: i32 = 3;

/// What the icon's two interfaces share with the UI side.
struct Shared {
    events: Sender<TrayEvent>,
    ctx: egui::Context,
    tip: Mutex<String>,
}

impl Shared {
    fn open(&self) {
        crate::come_forward(&self.ctx);
        let _ = self.events.send(TrayEvent::Open);
    }

    fn quit(&self) {
        let _ = self.events.send(TrayEvent::Quit);
        crate::repaint(&self.ctx);
    }
}

/// An image as StatusNotifierItem carries it: width, height, pixels.
type Pixmap = (i32, i32, Vec<u8>);

/// A tooltip: icon name, icon, title, text.
type ToolTip = (String, Vec<Pixmap>, String, String);

/// The logo, 64 pixels square, as the protocol wants it: ARGB32 in network
/// byte order.
fn pixmap() -> Pixmap {
    let rgba = include_bytes!("../../assets/logo/icon-64.rgba");
    let argb = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| [a, r, g, b])
        .collect();
    (64, 64, argb)
}

struct Item {
    shared: Arc<Shared>,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    fn category(&self) -> &str {
        "SystemServices"
    }

    #[zbus(property)]
    fn id(&self) -> &str {
        "anabolic"
    }

    #[zbus(property)]
    fn title(&self) -> &str {
        "Anabolic"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }

    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }

    #[zbus(property)]
    fn icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<Pixmap> {
        vec![pixmap()]
    }

    #[zbus(property)]
    fn tool_tip(&self) -> ToolTip {
        let tip = self
            .shared
            .tip
            .lock()
            .map(|t| t.clone())
            .unwrap_or_default();
        (String::new(), Vec::new(), "Anabolic".into(), tip)
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn menu(&self) -> zbus::zvariant::ObjectPath<'_> {
        zbus::zvariant::ObjectPath::from_static_str_unchecked(MENU_PATH)
    }

    /// A click on the icon.
    fn activate(&self, _x: i32, _y: i32) {
        self.shared.open();
    }

    /// A middle click.
    fn secondary_activate(&self, _x: i32, _y: i32) {
        self.shared.open();
    }

    fn context_menu(&self, _x: i32, _y: i32) {}

    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// One menu entry and its children: (ia{sv}av).
#[derive(serde::Serialize, Type, Value)]
struct Layout {
    id: i32,
    properties: HashMap<String, Value<'static>>,
    children: Vec<Value<'static>>,
}

fn entry(id: i32) -> HashMap<String, Value<'static>> {
    let mut p: HashMap<String, Value<'static>> = HashMap::new();
    match id {
        0 => {
            p.insert("children-display".into(), Value::from("submenu"));
        }
        OPEN => {
            p.insert("label".into(), Value::from("Open Anabolic"));
        }
        SEPARATOR => {
            p.insert("type".into(), Value::from("separator"));
        }
        QUIT => {
            p.insert("label".into(), Value::from("Quit Anabolic"));
        }
        _ => {}
    }
    p
}

fn layout(id: i32) -> Layout {
    let children = if id == 0 {
        [OPEN, SEPARATOR, QUIT]
            .into_iter()
            .map(|child| Value::from(layout(child)))
            .collect()
    } else {
        Vec::new()
    };
    Layout {
        id,
        properties: entry(id),
        children,
    }
}

struct Menu {
    shared: Arc<Shared>,
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    /// The menu never changes: revision 1, whatever part is asked for.
    fn get_layout(&self, parent: i32, _depth: i32, _properties: Vec<String>) -> (u32, Layout) {
        (1, layout(parent))
    }

    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        _properties: Vec<String>,
    ) -> Vec<(i32, HashMap<String, Value<'static>>)> {
        let ids = if ids.is_empty() {
            vec![0, OPEN, SEPARATOR, QUIT]
        } else {
            ids
        };
        ids.into_iter().map(|id| (id, entry(id))).collect()
    }

    fn get_property(&self, id: i32, name: &str) -> Value<'static> {
        entry(id).remove(name).unwrap_or_else(|| Value::from(""))
    }

    fn event(&self, id: i32, event: &str, _data: Value<'_>, _timestamp: u32) {
        if event != "clicked" {
            return;
        }
        match id {
            OPEN => self.shared.open(),
            QUIT => self.shared.quit(),
            _ => {}
        }
    }

    fn event_group(&self, events: Vec<(i32, String, Value<'_>, u32)>) -> Vec<i32> {
        for (id, event, data, timestamp) in events {
            self.event(id, &event, data, timestamp);
        }
        Vec::new()
    }

    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }

    #[zbus(signal)]
    async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;
}

fn register(conn: &Connection, name: &str) -> zbus::Result<()> {
    conn.call_method(
        Some(WATCHER),
        "/StatusNotifierWatcher",
        Some(WATCHER),
        "RegisterStatusNotifierItem",
        &name,
    )
    .map(|_| ())
}

pub struct Tray {
    conn: Connection,
    name: String,
    shared: Arc<Shared>,
    events: Receiver<TrayEvent>,
    /// The notifications shown, so a click on one of ours opens the window.
    shown: Arc<Mutex<Vec<u32>>>,
}

impl Tray {
    /// Puts the icon in the notification area. None when there is none (no
    /// StatusNotifierWatcher: GNOME without its AppIndicator extension):
    /// then closing the window must not hide it.
    pub fn start(ctx: egui::Context) -> Option<Tray> {
        let (events, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            events,
            ctx,
            tip: Mutex::new(String::new()),
        });
        let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
        let conn = zbus::blocking::connection::Builder::session()
            .ok()?
            .name(name.as_str())
            .ok()?
            .serve_at(
                ITEM_PATH,
                Item {
                    shared: shared.clone(),
                },
            )
            .ok()?
            .serve_at(
                MENU_PATH,
                Menu {
                    shared: shared.clone(),
                },
            )
            .ok()?
            .build()
            .ok()?;
        register(&conn, &name).ok()?;
        let shown = Arc::new(Mutex::new(Vec::new()));
        // Plasma restarted: the new watcher knows nothing of the icon.
        {
            let (conn, name) = (conn.clone(), name.clone());
            let _ = std::thread::Builder::new()
                .name("anabolic-tray-watcher".into())
                .spawn(move || {
                    let Ok(bus) = zbus::blocking::fdo::DBusProxy::new(&conn) else {
                        return;
                    };
                    let Ok(changes) = bus.receive_name_owner_changed_with_args(&[(0, WATCHER)])
                    else {
                        return;
                    };
                    for change in changes {
                        let back = change.args().is_ok_and(|a| a.new_owner().is_some());
                        if back {
                            let _ = register(&conn, &name);
                        }
                    }
                });
        }
        // A click on one of its notifications opens the window.
        {
            let (conn, shared, shown) = (conn.clone(), shared.clone(), shown.clone());
            let _ = std::thread::Builder::new()
                .name("anabolic-notifications".into())
                .spawn(move || {
                    let Ok(proxy) = zbus::blocking::Proxy::new(
                        &conn,
                        "org.freedesktop.Notifications",
                        "/org/freedesktop/Notifications",
                        "org.freedesktop.Notifications",
                    ) else {
                        return;
                    };
                    let Ok(clicks) = proxy.receive_signal("ActionInvoked") else {
                        return;
                    };
                    for click in clicks {
                        let Ok((id, action)) = click.body().deserialize::<(u32, String)>() else {
                            continue;
                        };
                        let ours = shown.lock().is_ok_and(|s| s.contains(&id));
                        if ours && action == "default" {
                            shared.open();
                        }
                    }
                });
        }
        Some(Tray {
            conn,
            name,
            shared,
            events: rx,
            shown,
        })
    }

    pub fn events(&self) -> impl Iterator<Item = TrayEvent> + '_ {
        self.events.try_iter()
    }

    /// The text shown when the pointer rests on the icon.
    pub fn tip(&self, text: &str) {
        if let Ok(mut tip) = self.shared.tip.lock() {
            if *tip == text {
                return;
            }
            *tip = text.to_string();
        }
        let _ = self.conn.emit_signal(
            None::<&str>,
            ITEM_PATH,
            "org.kde.StatusNotifierItem",
            "NewToolTip",
            &(),
        );
    }

    /// A notification from the icon; the desktop keeps it quiet while it is
    /// set not to disturb.
    pub fn notify(&self, title: &str, text: &str, warning: bool) {
        let (width, height, argb) = pixmap();
        // image-data is RGBA, unlike the icon's ARGB.
        let rgba: Vec<u8> = argb
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|&[a, r, g, b]| [r, g, b, a])
            .collect();
        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        hints.insert("urgency", Value::U8(if warning { 2 } else { 1 }));
        hints.insert("desktop-entry", Value::from(anabolic_core::APP_ID));
        hints.insert(
            "image-data",
            Value::from((width, height, width * 4, true, 8i32, 4i32, rgba)),
        );
        let reply = self.conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(
                "Anabolic",
                0u32,
                "",
                title,
                text,
                vec!["default", "Open Anabolic"],
                hints,
                -1i32,
            ),
        );
        if let Ok(reply) = reply
            && let Ok(id) = reply.body().deserialize::<u32>()
            && let Ok(mut shown) = self.shown.lock()
        {
            shown.push(id);
            let excess = shown.len().saturating_sub(16);
            shown.drain(..excess);
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // The icon goes with its name; closing the connection ends the
        // threads listening on it.
        let _ = self.conn.release_name(self.name.as_str());
        let _ = self.conn.clone().close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_has_its_entries_in_dbusmenus_shape() {
        assert_eq!(Layout::SIGNATURE.to_string(), "(ia{sv}av)");
        assert_eq!(layout(0).children.len(), 3);
        assert_eq!(
            entry(QUIT)
                .get("label")
                .and_then(|l| l.downcast_ref::<&str>().ok()),
            Some("Quit Anabolic")
        );
    }

    #[test]
    fn the_icon_is_argb() {
        let (w, h, argb) = pixmap();
        assert_eq!(argb.len(), (w * h * 4) as usize);
        let rgba = include_bytes!("../../assets/logo/icon-64.rgba");
        // Alpha first, then red, green, blue.
        assert_eq!(argb[..4], [rgba[3], rgba[0], rgba[1], rgba[2]]);
    }
}
