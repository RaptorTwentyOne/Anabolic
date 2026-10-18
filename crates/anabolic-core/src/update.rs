//! Updates from the project's GitHub releases: whether a newer one is out,
//! and putting its anabolic.exe in place of this copy's.
//!
//! A check is one HTTPS request to GitHub's API, through Windows' own HTTP
//! stack (its proxy settings and certificate store). It carries nothing
//! about this PC beyond what any download does: its address and
//! "Anabolic/<version>". A release's zip must match the SHA-256 GitHub lists
//! for it before anything is written.
//!
//! A running program's file cannot be overwritten, but it can be renamed:
//! the old one steps aside as anabolic.exe.old, the new one takes its name,
//! and the next start deletes the old one. The program goes on running from
//! the renamed file until it restarts.

use std::path::{Path, PathBuf};
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::Deserialize;
use windows::Win32::Networking::WinHttp::{
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

use crate::util::wide;

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
                        Request::Restart(args) => match relaunch(&exe, &args) {
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
    let _ = std::fs::remove_file(exe.with_extension("exe.old"));
    let _ = std::fs::remove_file(exe.with_extension("exe.new"));
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
    let (status, body) = get(
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
        .find(|a| a.name.ends_with("-windows-x64.zip"))
        .ok_or_else(|| format!("Anabolic {version} has no download for Windows x64"))?;
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

/// Downloads `release`, checks it, and puts its anabolic.exe where `exe`
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
    let (status, zip) = get(&release.zip, "", limit, |done| {
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
        .find(|(name, _)| name.eq_ignore_ascii_case("anabolic.exe"))
        .filter(|(_, data)| data.starts_with(b"MZ"))
        .ok_or("the release holds no anabolic.exe")?;
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
    let staged = exe.with_extension("exe.new");
    let old = exe.with_extension("exe.old");
    let refused = |e: std::io::Error| {
        format!(
            "Windows did not let Anabolic replace {}: {e}",
            exe.display()
        )
    };
    write_all(&staged, data).map_err(refused)?;
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

/// Starts `exe` with `--after <this process>` and `args`: it waits for
/// this one to exit before it takes over as the session's Anabolic.
fn relaunch(exe: &Path, args: &[String]) -> Result<(), String> {
    let pid = std::process::id().to_string();
    match std::process::Command::new(exe)
        .arg("--after")
        .arg(&pid)
        .args(args)
        .spawn()
    {
        Ok(_) => Ok(()),
        // ERROR_ELEVATION_REQUIRED: an administrator's copy running
        // without elevation; the new one asks through UAC as at any start.
        Err(e) if e.raw_os_error() == Some(740) => {
            let file = wide(&exe.to_string_lossy());
            let line = wide(&format!("--after {pid} {}", args.join(" ")));
            let result = unsafe {
                ShellExecuteW(
                    None,
                    w!("runas"),
                    PCWSTR(file.as_ptr()),
                    PCWSTR(line.as_ptr()),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                )
            };
            if result.0 as usize > 32 {
                Ok(())
            } else {
                Err("the new version could not start: elevation was cancelled".into())
            }
        }
        Err(e) => Err(format!("the new version could not start: {e}")),
    }
}

/// A WinHTTP handle, closed on drop.
struct Internet(*mut core::ffi::c_void);

impl Internet {
    fn open(handle: *mut core::ffi::c_void, doing: &str) -> Result<Self, String> {
        if handle.is_null() {
            Err(failure(doing))
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for Internet {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// Why the last WinHTTP call failed, in words: WinHTTP's errors have no
/// text in the system's message table.
fn failure(doing: &str) -> String {
    let e = windows::core::Error::from_thread();
    let why = match e.code().0 as u32 & 0xFFFF {
        12002 => "it took too long".to_string(),
        12007 => "the server could not be found (is this PC online?)".to_string(),
        12029 => "no connection could be made".to_string(),
        12030 | 12031 => "the connection was dropped".to_string(),
        12038 | 12044 | 12045 | 12057 | 12157 | 12175 => "the secure connection failed".to_string(),
        _ => crate::util::error_text(&e),
    };
    format!("{doing}: {why}")
}

/// GETs an https:// address, following redirects: the status and at most
/// `limit` bytes of the body. `progress` hears the bytes read so far.
fn get(
    url: &str,
    headers: &str,
    limit: usize,
    mut progress: impl FnMut(usize),
) -> Result<(u32, Vec<u8>), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("not an https address: {url}"))?;
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let doing = format!("Could not reach {host}");
    let agent = wide(&format!("Anabolic/{}", env!("CARGO_PKG_VERSION")));
    let (host, path) = (wide(host), wide(if path.is_empty() { "/" } else { path }));
    unsafe {
        let session = Internet::open(
            WinHttpOpen(
                PCWSTR(agent.as_ptr()),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            &doing,
        )?;
        // Name lookup, connecting, sending, each read.
        let _ = WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 30_000);
        let connection = Internet::open(
            WinHttpConnect(
                session.0,
                PCWSTR(host.as_ptr()),
                INTERNET_DEFAULT_HTTPS_PORT,
                0,
            ),
            &doing,
        )?;
        let request = Internet::open(
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                PCWSTR(path.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ),
            &doing,
        )?;
        let headers: Vec<u16> = headers.encode_utf16().collect();
        WinHttpSendRequest(
            request.0,
            (!headers.is_empty()).then_some(&headers[..]),
            None,
            0,
            0,
            0,
        )
        .map_err(|_| failure(&doing))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|_| failure(&doing))?;
        let mut status = 0u32;
        let mut len = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&raw mut status).cast()),
            &mut len,
            std::ptr::null_mut(),
        )
        .map_err(|_| failure(&doing))?;
        let mut body = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut read,
            )
            .map_err(|_| failure(&doing))?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&buf[..read as usize]);
            if body.len() > limit {
                return Err(format!("{doing}: the answer was larger than expected"));
            }
            progress(body.len());
        }
        Ok((status, body))
    }
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

#[cfg(test)]
mod tests {
    use windows::Win32::Foundation::FreeLibrary;
    use windows::Win32::System::LibraryLoader::{DONT_RESOLVE_DLL_REFERENCES, LoadLibraryExW};

    use super::*;

    /// A zip as Compress-Archive makes one: a folder entry, then files,
    /// deflated or (when tiny) stored.
    fn zip_of(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        let entries = std::iter::once(("pack/", &b""[..], false))
            .chain(files.iter().map(|&(n, d, deflate)| (n, d, deflate)));
        let mut count = 0u16;
        for (name, data, deflate) in entries {
            let packed = if deflate {
                miniz_oxide::deflate::compress_to_vec(data, 6)
            } else {
                data.to_vec()
            };
            let method: u16 = if deflate { 8 } else { 0 };
            let crc = crc32fast::hash(data);
            let offset = out.len() as u32;
            let mut common = Vec::new();
            common.extend_from_slice(&20u16.to_le_bytes());
            common.extend_from_slice(&0u16.to_le_bytes());
            common.extend_from_slice(&method.to_le_bytes());
            common.extend_from_slice(&[0; 4]);
            common.extend_from_slice(&crc.to_le_bytes());
            common.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            common.extend_from_slice(&(data.len() as u32).to_le_bytes());
            common.extend_from_slice(&(name.len() as u16).to_le_bytes());
            common.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&common);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&packed);
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&common);
            central.extend_from_slice(&[0; 10]);
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
            count += 1;
        }
        let at = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.1.1", "0.1.0"));
        assert!(newer("v0.10.0", "0.9.3"));
        assert!(newer("1.0", "0.99.99"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
        assert!(!newer("0.2.0-rc.1", "0.1.0"));
    }

    #[test]
    fn unpacks_what_it_packed() {
        let exe = b"MZ"
            .iter()
            .chain(&[7u8; 5000])
            .copied()
            .collect::<Vec<_>>();
        let zip = zip_of(&[
            (r"pack\anabolic.exe", &exe, true),
            ("pack/LICENSE-MIT", b"MIT License", false),
        ]);
        let files = unzip(&zip).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0], ("anabolic.exe".to_string(), exe));
        assert_eq!(
            files[1],
            ("LICENSE-MIT".to_string(), b"MIT License".to_vec())
        );
    }

    #[test]
    fn refuses_damaged_zips() {
        let mut zip = zip_of(&[("pack/anabolic.exe", &[1u8; 3000], true)]);
        assert!(unzip(&zip[..zip.len() - 30]).is_err());
        // A flipped byte in the packed data fails its CRC (or the inflate):
        // past the folder's entry and the file's header.
        let data = 30 + "pack/".len() + 30 + "pack/anabolic.exe".len();
        zip[data + 2] ^= 0xFF;
        assert!(unzip(&zip).is_err());
        assert!(unzip(b"").is_err());
    }

    #[test]
    fn replaces_a_running_program() {
        let dir = std::env::temp_dir().join(format!("anabolic-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("anabolic.exe");
        // A PE file mapped as an image, as a running program's is: it can
        // be renamed, not overwritten or deleted.
        std::fs::copy(r"C:\Windows\System32\version.dll", &exe).unwrap();
        let image = std::fs::read(&exe).unwrap();
        let path = wide(&exe.to_string_lossy());
        let module =
            unsafe { LoadLibraryExW(PCWSTR(path.as_ptr()), None, DONT_RESOLVE_DLL_REFERENCES) }
                .unwrap();
        assert!(std::fs::write(&exe, b"x").is_err());
        replace(&exe, b"new").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(exe.with_extension("exe.old")).unwrap(), image);
        // Again before a restart: the old one is still mapped, the file in
        // place is not.
        replace(&exe, b"newer").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"newer");
        unsafe {
            let _ = FreeLibrary(module);
        }
        clean_up(&exe);
        assert!(!exe.with_extension("exe.old").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Asks GitHub for real: `cargo test -p anabolic-core latest_release
    /// -- --ignored`.
    #[test]
    #[ignore]
    fn latest_release() {
        let release = check("0.0.1").unwrap().expect("a release");
        println!("{release:?}");
        assert!(release.sha256.is_some());
        assert!(newer(&release.version, "0.0.1"));
        assert_eq!(check(&release.version).unwrap().map(|r| r.version), None);
    }

    /// Downloads and unpacks the real zip into a folder of its own:
    /// `cargo test -p anabolic-core installs_the_latest -- --ignored`.
    #[test]
    #[ignore]
    fn installs_the_latest_release() {
        let dir = std::env::temp_dir().join(format!("anabolic-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("anabolic.exe");
        std::fs::write(&exe, b"MZ old").unwrap();
        let release = check("0.0.1").unwrap().expect("a release");
        let mut last = (0, 0);
        install(&release, &exe, |done, total| last = (done, total)).unwrap();
        assert_eq!(last, (release.size, release.size));
        let new = std::fs::read(&exe).unwrap();
        assert!(new.starts_with(b"MZ") && new.len() > 1 << 20);
        // Only the program: nothing else of ours lay beside it.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
