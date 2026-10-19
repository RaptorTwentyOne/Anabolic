//! Updates from GitHub's releases: is there a newer one, fetch it, check it
//! against the SHA-256 GitHub lists for it, and put it in place of the
//! running executable for the next start.

use std::path::{Path, PathBuf};
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::Deserialize;

#[cfg_attr(windows, path = "windows/update.rs")]
#[cfg_attr(target_os = "linux", path = "linux/update.rs")]
mod imp;

/// A release on GitHub, newer than this copy.
#[derive(Clone, Debug)]
pub struct Release {
    /// "0.1.1": the tag without its "v".
    pub version: String,
    /// The release's page, with its notes.
    pub page: String,
    zip: String,
    size: u64,
    /// As GitHub lists it; releases from before it did have none.
    sha256: Option<String>,
}

pub enum Event {
    /// A release newer than the running (or last installed) one, none, or
    /// why the check failed.
    Checked(Result<Option<Release>, String>),
    /// Downloading: bytes so far, of how many.
    Progress(u64, u64),
    /// The release's anabolic.exe is in place: the next start runs it.
    Installed(Release),
    InstallFailed(Release, String),
    /// The new copy has started and waits for this one to quit.
    Restarting,
    RestartFailed(String),
}

enum Request {
    Check,
    Install(Release),
    Restart(Vec<String>),
}

/// The update thread: checks, downloads and restarts happen there, never
/// on the caller's.
pub struct Updater {
    tx: Sender<Request>,
}

impl Updater {
    /// `current`: the version running now. `exe`: the file to replace (see
    /// [`this_exe`]). `notify` runs after every event.
    pub fn start(
        current: String,
        exe: PathBuf,
        notify: impl Fn() + Send + 'static,
    ) -> (Self, Receiver<Event>) {
        let (tx, rx) = unbounded::<Request>();
        let (out_tx, out_rx) = unbounded();
        let spawned = thread::Builder::new()
            .name("anabolic-update".into())
            .spawn(move || {
                clean_up(&exe);
                // What a check compares against: after an install, the
                // installed release, not the one still running.
                let mut current = current;
                let send = |event: Event| {
                    let sent = out_tx.send(event).is_ok();
                    notify();
                    sent
                };
                for request in rx {
                    let event = match request {
                        Request::Check => Event::Checked(check(&current)),
                        Request::Install(release) => {
                            let mut percent = u64::MAX;
                            let result = install(&release, &exe, |done, total| {
                                // A step per percent, not one per read.
                                let now = done * 100 / total.max(1);
                                if now != percent {
                                    percent = now;
                                    send(Event::Progress(done, total));
                                }
                            });
                            match result {
                                Ok(()) => {
                                    current.clone_from(&release.version);
                                    Event::Installed(release)
                                }
                                Err(e) => Event::InstallFailed(release, e),
                            }
                        }
                        Request::Restart(args) => match imp::relaunch(&exe, &args) {
                            Ok(()) => Event::Restarting,
                            Err(e) => Event::RestartFailed(e),
                        },
                    };
                    if !send(event) {
                        return;
                    }
                }
            });
        if let Err(e) = spawned {
            eprintln!("anabolic: no update thread: {e}");
        }
        (Self { tx }, out_rx)
    }

    pub fn check(&self) {
        let _ = self.tx.send(Request::Check);
    }

    pub fn install(&self, release: Release) {
        let _ = self.tx.send(Request::Install(release));
    }

    /// Starts the executable now in place with `args`, to take over once
    /// this process has exited.
    pub fn restart(&self, args: Vec<String>) {
        let _ = self.tx.send(Request::Restart(args));
    }
}

/// This program's file, with links followed: winget starts it through one
/// in its Links folder, and the link must stay as it is.
pub fn this_exe() -> Option<PathBuf> {
    let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    // canonicalize gives \\?\C:\...; a plain C:\... reads better, and
    // works everywhere a path does.
    let text = exe.to_str()?;
    Some(match text.strip_prefix(r"\\?\") {
        Some(plain) if plain.as_bytes().get(1) == Some(&b':') => PathBuf::from(plain),
        _ => exe,
    })
}

/// Built by cargo (it sits in a target folder, which cargo marks with
/// CACHEDIR.TAG): such a copy is updated from its source, not replaced by
/// a release.
pub fn from_source(exe: &Path) -> bool {
    exe.ancestors()
        .skip(1)
        .take(3)
        .any(|dir| dir.join("CACHEDIR.TAG").exists())
}

/// Deletes what the last update left: the old executable it moved aside,
/// and a download it could not put in place.
pub fn clean_up(exe: &Path) {
    let _ = std::fs::remove_file(aside(exe, "old"));
    let _ = std::fs::remove_file(aside(exe, "new"));
}

/// `anabolic.exe.old` beside `anabolic.exe`; `anabolic.old` beside `anabolic`.
fn aside(exe: &Path, what: &str) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(what);
    exe.with_file_name(name)
}

/// "RaptorTwentyOne/Anabolic", from the workspace's repository address.
fn repository() -> &'static str {
    env!("CARGO_PKG_REPOSITORY").trim_start_matches("https://github.com/")
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    /// "sha256:<hex>".
    #[serde(default)]
    digest: Option<String>,
}

/// The latest release, when it is newer than `current`.
pub fn check(current: &str) -> Result<Option<Release>, String> {
    let url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        repository()
    );
    let (status, body) = imp::get(
        &url,
        "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28",
        1 << 20,
        |_| {},
    )?;
    match status {
        200 => {}
        // Nothing published yet.
        404 => return Ok(None),
        403 | 429 => return Err("GitHub asks to wait before the next check".into()),
        _ => return Err(format!("GitHub answered {status}")),
    }
    let release: ApiRelease = serde_json::from_slice(&body)
        .map_err(|e| format!("GitHub's answer was unexpected: {e}"))?;
    let version = release.tag_name.trim_start_matches('v').to_string();
    if !newer(&version, current) {
        return Ok(None);
    }
    let asset = release
        .assets
        .into_iter()
        .find(|a| a.name.ends_with(imp::ASSET))
        .ok_or_else(|| format!("Anabolic {version} has no download for {}", imp::PLATFORM))?;
    Ok(Some(Release {
        version,
        page: release.html_url,
        zip: asset.browser_download_url,
        size: asset.size,
        sha256: asset
            .digest
            .and_then(|d| Some(d.strip_prefix("sha256:")?.to_ascii_lowercase())),
    }))
}

/// Whether `version` comes after `current` ("0.10.0" after "0.9.3"). A
/// pre-release ("0.2.0-rc.1") is never newer.
pub fn newer(version: &str, current: &str) -> bool {
    let parts = |v: &str| -> Option<Vec<u64>> {
        v.trim_start_matches('v')
            .split('.')
            .map(|p| p.parse().ok())
            .collect()
    };
    let (Some(mut new), Some(mut old)) = (parts(version), parts(current)) else {
        return false;
    };
    let len = new.len().max(old.len());
    new.resize(len, 0);
    old.resize(len, 0);
    new > old
}

/// Downloads `release`, checks it, and puts its executable where `exe`
/// is. The license and notice files that came with an earlier release are
/// replaced too, where they lie beside it. `progress` hears the bytes so
/// far and the total.
fn install(
    release: &Release,
    exe: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    let want = release
        .sha256
        .as_deref()
        .ok_or("GitHub lists no checksum for this release, so it is not installed")?;
    let limit = (release.size as usize).saturating_add(1 << 20);
    let (status, zip) = imp::get(&release.zip, "", limit, |done| {
        progress(done as u64, release.size)
    })?;
    if status != 200 {
        return Err(format!("the download failed: GitHub answered {status}"));
    }
    let have = crate::filefacts::sha256_of(&zip[..]).ok_or("its checksum could not be computed")?;
    if have != want {
        return Err("the download does not match its checksum; nothing was changed".into());
    }
    let files = unzip(&zip)?;
    let new = files
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(imp::EXECUTABLE))
        .filter(|(_, data)| data.starts_with(imp::MAGIC))
        .ok_or_else(|| format!("the release holds no {}", imp::EXECUTABLE))?;
    replace(exe, &new.1)?;
    // The others only where they came from us: a README.md of someone
    // else's in the same folder stays as it is.
    if let Some(dir) = exe.parent() {
        for (name, data) in &files {
            let path = dir.join(name);
            if path == exe {
                continue;
            }
            if let Ok(old) = std::fs::read(&path)
                && old.len() >= 64
                && data.len() >= 64
                && old[..64] == data[..64]
            {
                let _ = std::fs::write(&path, data);
            }
        }
    }
    Ok(())
}

/// Puts `data` in place of `exe`, running or not.
fn replace(exe: &Path, data: &[u8]) -> Result<(), String> {
    let staged = aside(exe, "new");
    let old = aside(exe, "old");
    let refused = |e: std::io::Error| {
        format!(
            "{} did not let Anabolic replace {}: {e}",
            imp::SYSTEM,
            exe.display()
        )
    };
    write_all(&staged, data).map_err(refused)?;
    imp::make_runnable(&staged).map_err(refused)?;
    // Not running (installed once already, still running the copy before):
    // replaced as it is.
    if std::fs::rename(&staged, exe).is_ok() {
        return Ok(());
    }
    // An earlier update's leftover, if the last start could not delete it.
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(exe, &old) {
        let _ = std::fs::remove_file(&staged);
        return Err(refused(e));
    }
    if let Err(e) = std::fs::rename(&staged, exe) {
        let _ = std::fs::rename(&old, exe);
        let _ = std::fs::remove_file(&staged);
        return Err(refused(e));
    }
    Ok(())
}

/// Writes the file and waits for it to be on the disk.
fn write_all(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(data)?;
    file.sync_all()
}

/// The files of a zip archive, by name without their folders: stored or
/// deflated entries, each checked against its CRC-32.
fn unzip(zip: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let bad = || "the release's zip is damaged".to_string();
    let u16_at = |at: usize| -> Result<usize, String> {
        let b = zip.get(at..at + 2).ok_or_else(bad)?;
        Ok(u16::from_le_bytes([b[0], b[1]]) as usize)
    };
    let u32_at = |at: usize| -> Result<usize, String> {
        let b = zip.get(at..at + 4).ok_or_else(bad)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    // The end of central directory record: 22 bytes, and a comment of up
    // to 64 KB after it.
    let last = zip.len().checked_sub(22).ok_or_else(bad)?;
    let end = (last.saturating_sub(0xFFFF)..=last)
        .rev()
        .find(|&at| u32_at(at) == Ok(0x0605_4b50))
        .ok_or_else(bad)?;
    let count = u16_at(end + 10)?;
    let mut at = u32_at(end + 16)?;
    let mut files = Vec::with_capacity(count);
    for _ in 0..count {
        if u32_at(at)? != 0x0201_4b50 {
            return Err(bad());
        }
        let method = u16_at(at + 10)?;
        let crc = u32_at(at + 16)? as u32;
        let packed = u32_at(at + 20)?;
        let size = u32_at(at + 24)?;
        let name_len = u16_at(at + 28)?;
        let local = u32_at(at + 42)?;
        let name = zip.get(at + 46..at + 46 + name_len).ok_or_else(bad)?;
        let name = String::from_utf8_lossy(name).into_owned();
        at += 46 + name_len + u16_at(at + 30)? + u16_at(at + 32)?;
        if name.ends_with(['/', '\\']) {
            continue;
        }
        if u32_at(local)? != 0x0403_4b50 {
            return Err(bad());
        }
        let start = local + 30 + u16_at(local + 26)? + u16_at(local + 28)?;
        let data = zip.get(start..start + packed).ok_or_else(bad)?;
        let data = match method {
            0 => data.to_vec(),
            8 => {
                miniz_oxide::inflate::decompress_to_vec_with_limit(data, size).map_err(|_| bad())?
            }
            _ => {
                return Err(format!(
                    "the release's zip is packed in a way Anabolic cannot read ({method})"
                ));
            }
        };
        if data.len() != size || crc32fast::hash(&data) != crc {
            return Err(bad());
        }
        let file = name.rsplit(['/', '\\']).next().unwrap_or(&name).to_string();
        files.push((file, data));
    }
    Ok(files)
}
