//! Making the desktop know Anabolic: `anabolic --install` puts a .desktop
//! file and the icon in the user's data folder, so the application menu
//! lists it and the taskbar shows its icon (a Wayland compositor finds a
//! window's icon through the .desktop file named like its app id);
//! `--uninstall` takes them away. Nothing outside the user's home is
//! touched.

use std::path::{Path, PathBuf};

/// The app id the window carries, and the .desktop file's name.
pub const APP_ID: &str = "anabolic";

/// $XDG_DATA_HOME, usually ~/.local/share.
fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".local/share")))
}

/// The files `--install` writes, with what goes in them.
fn files(exe: &Path) -> Option<Vec<(PathBuf, Vec<u8>)>> {
    let data = data_home()?;
    let entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Anabolic\n\
         GenericName=Task Manager\n\
         Comment=See who and what keeps the computer busy, and why\n\
         Exec=\"{}\" %U\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=System;Monitor;\n\
         Keywords=task;process;performance;monitor;cpu;memory;disk;network;\n\
         StartupWMClass={APP_ID}\n",
        exe.display()
    );
    let icons = data.join("icons/hicolor");
    Some(vec![
        (
            data.join(format!("applications/{APP_ID}.desktop")),
            entry.into_bytes(),
        ),
        (
            icons.join(format!("scalable/apps/{APP_ID}.svg")),
            include_bytes!("../../assets/logo/logo.svg").to_vec(),
        ),
        (
            icons.join(format!("64x64/apps/{APP_ID}.png")),
            include_bytes!("../../assets/logo/icon-64.png").to_vec(),
        ),
    ])
}

/// `--install` and `--uninstall`: what was done, or why not.
pub fn run(install: bool) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let files = files(&exe).ok_or("no home folder to install into")?;
    let mut done = Vec::new();
    for (path, data) in files {
        if install {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            std::fs::write(&path, data).map_err(|e| format!("{}: {e}", path.display()))?;
            done.push(format!("wrote {}", path.display()));
        } else if std::fs::remove_file(&path).is_ok() {
            done.push(format!("removed {}", path.display()));
        }
    }
    if done.is_empty() {
        done.push("nothing was installed".into());
    }
    Ok(done.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_the_program_where_the_menu_looks() {
        let files = files(Path::new("/opt/anabolic/anabolic")).unwrap();
        let desktop = String::from_utf8(files[0].1.clone()).unwrap();
        assert!(files[0].0.ends_with("applications/anabolic.desktop"));
        assert!(desktop.contains("Exec=\"/opt/anabolic/anabolic\" %U"));
        assert!(desktop.contains("StartupWMClass=anabolic"));
        assert!(
            files[1]
                .0
                .ends_with("icons/hicolor/scalable/apps/anabolic.svg")
        );
        assert!(files[1].1.starts_with(b"<svg"));
        assert!(files[2].1.starts_with(b"\x89PNG"));
    }
}
