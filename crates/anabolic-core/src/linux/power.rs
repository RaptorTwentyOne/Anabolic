//! Power profiles for game mode, through power-profiles-daemon (the
//! profiles KDE's and GNOME's battery menus switch): Performance while a
//! game runs, and the profile it replaced afterwards.

use zbus::blocking::{Connection, Proxy};

/// The daemon's current name, and the one it had before version 0.20.
const SERVICES: [(&str, &str); 2] = [
    (
        "org.freedesktop.UPower.PowerProfiles",
        "/org/freedesktop/UPower/PowerProfiles",
    ),
    ("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles"),
];

const PERFORMANCE: &str = "performance";

fn proxy(conn: &Connection) -> Result<Proxy<'_>, String> {
    let mut last = String::from("power-profiles-daemon is not running");
    for (service, path) in SERVICES {
        match Proxy::new(conn, service, path, service) {
            Ok(proxy) if proxy.get_property::<String>("ActiveProfile").is_ok() => {
                return Ok(proxy);
            }
            Ok(_) => {}
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// A profile's name as the desktop shows it.
pub fn name(profile: &str) -> String {
    match profile {
        "power-saver" => "Power Save".into(),
        "balanced" => "Balanced".into(),
        PERFORMANCE => "Performance".into(),
        other => other.into(),
    }
}

/// Switches to Performance. The profile it replaced (id, name), or None
/// when Performance was on already.
pub fn for_game() -> Result<Option<(String, String)>, String> {
    let conn = Connection::system().map_err(|e| e.to_string())?;
    let proxy = proxy(&conn)?;
    let active: String = proxy
        .get_property("ActiveProfile")
        .map_err(|e| e.to_string())?;
    if active == PERFORMANCE {
        return Ok(None);
    }
    let offered: Vec<std::collections::HashMap<String, zbus::zvariant::OwnedValue>> =
        proxy.get_property("Profiles").map_err(|e| e.to_string())?;
    let has = offered.iter().any(|p| {
        p.get("Profile")
            .and_then(|v| String::try_from(v.clone()).ok())
            .is_some_and(|name| name == PERFORMANCE)
    });
    if !has {
        return Err("this computer offers no Performance profile".into());
    }
    proxy
        .set_property("ActiveProfile", PERFORMANCE)
        .map_err(|e| e.to_string())?;
    let shown = name(&active);
    Ok(Some((active, shown)))
}

/// Puts `previous` back, unless someone chose another profile meanwhile
/// (then None).
pub fn restore(previous: &str) -> Result<Option<String>, String> {
    let conn = Connection::system().map_err(|e| e.to_string())?;
    let proxy = proxy(&conn)?;
    let active: String = proxy
        .get_property("ActiveProfile")
        .map_err(|e| e.to_string())?;
    if active != PERFORMANCE {
        return Ok(None);
    }
    proxy
        .set_property("ActiveProfile", previous)
        .map_err(|e| e.to_string())?;
    Ok(Some(name(previous)))
}
