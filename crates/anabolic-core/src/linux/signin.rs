//! Starting at sign-in on Linux: an autostart entry in the user's own
//! folder, which every desktop starts as the session begins.

use std::path::{Path, PathBuf};

use super::SignIn;

pub(super) fn state(_exe: &Path) -> SignIn {
    state_in(&config_home())
}

pub(super) fn set(exe: &Path, on: bool) -> Result<(), String> {
    set_in(&config_home(), exe, on)
}

/// No installer on Linux to clean up after.
pub(super) fn forget_everyone() -> Result<(), String> {
    Ok(())
}

/// ~/.config, or where XDG_CONFIG_HOME says.
fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".config")
        })
}

fn entry_path(config: &Path) -> PathBuf {
    config
        .join("autostart")
        .join(format!("{}.desktop", crate::APP_ID))
}

/// On while the entry is there and not turned off (the Startup page and
/// the desktops' own settings turn an entry off with Hidden=true or
/// X-GNOME-Autostart-enabled=false).
fn state_in(config: &Path) -> SignIn {
    let Ok(text) = std::fs::read_to_string(entry_path(config)) else {
        return SignIn::Off;
    };
    let off = text.lines().any(|line| {
        let line = line.replace(' ', "").to_ascii_lowercase();
        line == "hidden=true" || line == "x-gnome-autostart-enabled=false"
    });
    if off { SignIn::Off } else { SignIn::On }
}

fn set_in(config: &Path, exe: &Path, on: bool) -> Result<(), String> {
    let path = entry_path(config);
    let failed = |e: std::io::Error| format!("{}: {e}", path.display());
    if !on {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(failed(e)),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    std::fs::write(&path, entry(exe)).map_err(failed)
}

/// The entry: the program, quoted as the Desktop Entry specification says,
/// in the notification area.
fn entry(exe: &Path) -> String {
    let mut quoted = String::from("\"");
    for c in exe.to_string_lossy().chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Anabolic\n\
         Comment=Performance monitoring and diagnostics, in the notification area\n\
         Exec={quoted} --tray\n\
         Icon={}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        crate::APP_ID
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_comes_and_goes() {
        let config = std::env::temp_dir().join(format!("anabolic-signin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&config);
        let exe = Path::new("/opt/My Tools/anabolic");
        assert_eq!(state_in(&config), SignIn::Off);
        set_in(&config, exe, true).unwrap();
        assert_eq!(state_in(&config), SignIn::On);
        let text = std::fs::read_to_string(entry_path(&config)).unwrap();
        assert!(
            text.contains("Exec=\"/opt/My Tools/anabolic\" --tray\n"),
            "{text}"
        );
        // Turned off from elsewhere, the entry stays but does not run.
        std::fs::write(entry_path(&config), format!("{text}Hidden=true\n")).unwrap();
        assert_eq!(state_in(&config), SignIn::Off);
        set_in(&config, exe, true).unwrap();
        assert_eq!(state_in(&config), SignIn::On);
        set_in(&config, exe, false).unwrap();
        assert_eq!(state_in(&config), SignIn::Off);
        set_in(&config, exe, false).unwrap();
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn quotes_what_the_specification_reserves() {
        let text = entry(Path::new("/home/a\"b$c/anabolic"));
        assert!(
            text.contains(r#"Exec="/home/a\"b\$c/anabolic" --tray"#),
            "{text}"
        );
    }
}
