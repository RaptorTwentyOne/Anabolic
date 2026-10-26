// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

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
//!
//! A copy the installer put in Program Files steps aside the same way, and
//! the release's installer (msiexec, without a window) puts the new one in
//! its place.

use std::path::Path;

use windows::Win32::Networking::WinHttp::{
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

use crate::util::wide;

/// The release's file for this platform, and its executable.
pub(super) const ASSET: &str = "-windows-x64.zip";
pub(super) const PLATFORM: &str = "Windows x64";
pub(super) const SYSTEM: &str = "Windows";
pub(super) const EXECUTABLE: &str = "anabolic.exe";
pub(super) const MAGIC: &[u8] = b"MZ";

/// The release's installer, for copies it installed.
pub(super) const PACKAGE: Option<&str> = Some("-windows-x64.msi");

/// Any file runs on Windows.
pub(super) fn make_runnable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Whether `exe` lies in the folder the installer put Anabolic in: the
/// installer writes it down for this.
pub(super) fn installed(exe: &Path) -> bool {
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"Software\RaptorTwentyOne\Anabolic"),
            w!("InstallFolder"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return false;
    }
    let folder = crate::util::from_wide(&buf);
    let folder = folder.trim_end_matches('\\');
    exe.parent()
        .and_then(|dir| dir.to_str())
        .is_some_and(|dir| dir.trim_end_matches('\\').eq_ignore_ascii_case(folder))
}

/// The installer puts Anabolic in Program Files, for every user of the PC:
/// only an administrator may change it.
pub(super) fn may_install_package() -> Result<(), String> {
    if crate::hardening::is_elevated() {
        Ok(())
    } else {
        Err(
            "Anabolic is installed for every user of this PC, so only an administrator can \
             update it: start Anabolic as administrator, or run the new release's installer"
                .into(),
        )
    }
}

/// `path` open for reading, and while it is, for nobody else to write,
/// rename or delete.
pub(super) fn hold(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_SHARE_READ only.
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(path)
}

/// Runs the installer at `package` without a window, as an update
/// (ANABOLIC_UPDATE=1: it leaves the running Anabolic alone), and waits
/// for it. Its log goes to `log`.
pub(super) fn run_package(package: &Path, log: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let msiexec = std::env::var_os("SystemRoot")
        .map_or_else(|| r"C:\Windows".into(), std::path::PathBuf::from)
        .join(r"System32\msiexec.exe");
    let status = std::process::Command::new(msiexec)
        .arg("/i")
        .arg(package)
        .args(["/qn", "/norestart", "ANABOLIC_UPDATE=1", "/l*v"])
        .arg(log)
        // CREATE_NO_WINDOW
        .creation_flags(0x0800_0000)
        .status()
        .map_err(|e| format!("the installer could not start: {e}"))?;
    match status.code() {
        // Done; done and Windows wants a restart (it does not, unless
        // something else of ours was in use).
        Some(0 | 3010) => Ok(()),
        Some(1618) => {
            Err("Windows is installing another program; it installs one at a time".into())
        }
        Some(code) => {
            let why = crate::util::error_text(&windows::core::Error::from_hresult(
                windows::core::HRESULT::from_win32(code as u32),
            ));
            Err(format!(
                "the installer stopped: {why}. What it did is in {}",
                log.display()
            ))
        }
        None => Err("the installer stopped without an answer".into()),
    }
}

/// A rename refused because another program has the file open (sharing or
/// lock violation), or for the moment (access denied): worth another try.
pub(super) fn busy(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

/// Starts `exe` with `--after <this process>` and `args`: it waits for
/// this one to exit before it takes over as the session's Anabolic.
pub(super) fn relaunch(exe: &Path, args: &[String]) -> Result<(), String> {
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
pub(super) fn get(
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

#[cfg(test)]
mod tests {
    use windows::Win32::Foundation::FreeLibrary;
    use windows::Win32::System::LibraryLoader::{DONT_RESOLVE_DLL_REFERENCES, LoadLibraryExW};

    use super::*;
    use crate::update::*;

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

    /// The zip for every copy and the installer for installed ones, among
    /// whatever else a release holds.
    #[test]
    fn finds_the_zip_and_the_installer() {
        let asset = |name: &str, size: u64, digest: &str| {
            format!(
                r#"{{"name": "{name}", "browser_download_url": "https://example.invalid/{name}",
                    "size": {size}, "digest": "sha256:{digest}"}}"#
            )
        };
        let answer = |assets: &[String]| {
            format!(
                r#"{{"tag_name": "v0.3.0", "html_url": "https://example.invalid/v0.3.0",
                    "assets": [{}]}}"#,
                assets.join(",")
            )
        };
        let all = answer(&[
            asset("anabolic-v0.3.0-linux-x64.zip", 1, "AA"),
            asset("anabolic-v0.3.0-windows-x64.msi", 2, "BB"),
            asset("anabolic-v0.3.0-windows-x64.zip", 3, "CC"),
        ]);
        let release = from_answer(all.as_bytes(), "0.2.1").unwrap().unwrap();
        assert_eq!(release.version, "0.3.0");
        assert!(release.zip.url.ends_with("-windows-x64.zip"));
        assert_eq!(
            (release.zip.size, release.zip.sha256.as_deref()),
            (3, Some("cc"))
        );
        let package = release.package.unwrap();
        assert!(package.url.ends_with("-windows-x64.msi"));
        assert_eq!((package.size, package.sha256.as_deref()), (2, Some("bb")));
        // Saved under the release's name for it, if that is a plain one.
        assert_eq!(
            package_name(&package.name),
            "anabolic-v0.3.0-windows-x64.msi"
        );
        for odd in ["", "..\\x.msi", "a/b.msi", "a b.msi", ".msi", "x.exe"] {
            assert_eq!(package_name(odd), "anabolic.msi", "{odd}");
        }
        assert!(from_answer(all.as_bytes(), "0.3.0").unwrap().is_none());
        // A release from before the installer: the zip for every copy.
        let zip_only = answer(&[asset("anabolic-v0.3.0-windows-x64.zip", 3, "CC")]);
        let release = from_answer(zip_only.as_bytes(), "0.2.1").unwrap().unwrap();
        assert!(release.package.is_none());
        let none = answer(&[asset("anabolic-v0.3.0-windows-x64.msi", 2, "BB")]);
        assert!(from_answer(none.as_bytes(), "0.2.1").is_err());
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

    /// A file held for a moment, as SmartScreen or a virus scanner holds a
    /// program it reads, is waited out.
    #[test]
    fn waits_out_a_file_held_for_a_moment() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = std::env::temp_dir().join(format!("anabolic-held-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (from, to) = (dir.join("anabolic.exe"), dir.join("anabolic.exe.old"));
        std::fs::write(&from, b"x").unwrap();
        // Shared for reading only: nobody may move it while it is open.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&from)
            .unwrap();
        let e = std::fs::rename(&from, &to).unwrap_err();
        assert!(busy(&e), "{e}");
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(600));
            drop(held);
        });
        rename_patiently(&from, &to).unwrap();
        release.join().unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Asks GitHub for real: `cargo test -p anabolic-core latest_release
    /// -- --ignored`.
    #[test]
    #[ignore]
    fn latest_release() {
        let release = check("0.0.1").unwrap().expect("a release");
        println!("{release:?}");
        assert!(release.zip.sha256.is_some());
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
        assert_eq!(last, (release.zip.size, release.zip.size));
        let new = std::fs::read(&exe).unwrap();
        assert!(new.starts_with(b"MZ") && new.len() > 1 << 20);
        // Only the program: nothing else of ours lay beside it.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An installer that does not match its checksum is never run, and
    /// the program stays where it was.
    #[test]
    fn refuses_an_installer_that_does_not_match() {
        let dir = std::env::temp_dir().join(format!("anabolic-package-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("anabolic.exe");
        std::fs::write(&exe, b"MZ old").unwrap();
        let package = Download {
            name: "anabolic-v9.9.9-windows-x64.msi".into(),
            url: String::new(),
            size: 17,
            sha256: Some("0".repeat(64)),
        };
        let e = install_package(b"not the installer", &package, &exe).unwrap_err();
        assert!(e.contains("changed after it was checked"), "{e}");
        assert_eq!(std::fs::read(&exe).unwrap(), b"MZ old");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Updates the installed copy in Program Files with an installer, as
    /// an update does while it runs. Elevated, with Anabolic installed and
    /// ANABOLIC_TEST_MSI naming a newer anabolic.msi:
    /// `cargo test -p anabolic-core updates_an_installed_copy -- --ignored`.
    #[test]
    #[ignore]
    fn updates_an_installed_copy() {
        let msi = std::fs::read(std::env::var("ANABOLIC_TEST_MSI").unwrap()).unwrap();
        let exe = std::path::PathBuf::from(std::env::var("ProgramFiles").unwrap())
            .join(r"Anabolic\anabolic.exe");
        assert!(installed(&exe), "not installed: {}", exe.display());
        may_install_package().unwrap();
        let before = std::fs::read(&exe).unwrap();
        // Mapped as a running program's is.
        let path = wide(&exe.to_string_lossy());
        let module =
            unsafe { LoadLibraryExW(PCWSTR(path.as_ptr()), None, DONT_RESOLVE_DLL_REFERENCES) }
                .unwrap();
        let package = Download {
            name: "anabolic-test-windows-x64.msi".into(),
            url: String::new(),
            size: msi.len() as u64,
            sha256: crate::filefacts::sha256_of(&msi[..]),
        };
        install_package(&msi, &package, &exe).unwrap();
        assert_ne!(std::fs::read(&exe).unwrap(), before);
        assert_eq!(
            std::fs::read(exe.with_extension("exe.old")).unwrap(),
            before
        );
        assert!(installed(&exe));
        unsafe {
            let _ = FreeLibrary(module);
        }
        clean_up(&exe);
        assert!(!exe.with_extension("exe.old").exists());
    }
}
