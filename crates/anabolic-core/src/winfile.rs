//! File- and account-level lookups used by the enrichment worker: NT to DOS
//! path translation, version resources, shell icons and SID names.

use core::ffi::c_void;
use core::mem::size_of;

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HBITMAP, ReleaseDC,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{LookupAccountSidW, PSID, SID_NAME_USE};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_NORMAL, GetFileVersionInfoSizeW, GetFileVersionInfoW, GetLogicalDrives,
    QueryDosDeviceW, VS_FIXEDFILEINFO, VerQueryValueW,
};
use windows::Win32::UI::Shell::{
    SHDefExtractIconW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows::core::{PCWSTR, PWSTR};

use crate::ntapi;
use crate::util::{from_wide, wide};

/// Maps `\Device\HarddiskVolumeN` prefixes to drive letters.
#[derive(Default)]
pub struct DeviceMap {
    prefixes: Vec<(String, String)>,
}

impl DeviceMap {
    pub fn build() -> Self {
        let mask = unsafe { GetLogicalDrives() };
        let mut prefixes = Vec::new();
        let mut target = vec![0u16; 1024];
        for i in 0..26u8 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let drive = format!("{}:", (b'A' + i) as char);
            let name = wide(&drive);
            let n = unsafe { QueryDosDeviceW(PCWSTR(name.as_ptr()), Some(&mut target)) };
            if n > 0 {
                prefixes.push((from_wide(&target), drive));
            }
        }
        Self { prefixes }
    }

    pub fn to_dos_path(&self, nt_path: &str) -> Option<String> {
        if let Some(rest) = nt_path.strip_prefix(r"\SystemRoot\") {
            return std::env::var("SystemRoot")
                .ok()
                .map(|root| format!(r"{root}\{rest}"));
        }
        if let Some(rest) = nt_path.strip_prefix(r"\??\") {
            return Some(rest.to_string());
        }
        self.prefixes.iter().find_map(|(device, drive)| {
            let head = nt_path.get(..device.len())?;
            let tail = &nt_path[device.len()..];
            (head.eq_ignore_ascii_case(device) && tail.starts_with('\\'))
                .then(|| format!("{drive}{tail}"))
        })
    }
}

/// Image path of any process, without opening it. Returns an NT path.
pub fn nt_image_path(pid: u32) -> Option<String> {
    let mut buf = vec![0u16; 512];
    for _ in 0..2 {
        let mut info = ntapi::SYSTEM_PROCESS_ID_INFORMATION {
            ProcessId: pid as usize,
            ImageName: ntapi::UNICODE_STRING {
                Length: 0,
                MaximumLength: (buf.len() * 2).min(u16::MAX as usize - 1) as u16,
                Buffer: buf.as_mut_ptr(),
            },
        };
        let status = unsafe {
            ntapi::NtQuerySystemInformation(
                ntapi::SystemProcessIdInformation,
                (&raw mut info).cast(),
                size_of::<ntapi::SYSTEM_PROCESS_ID_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        };
        if ntapi::nt_success(status) {
            let len = (info.ImageName.Length as usize / 2).min(buf.len());
            let path = String::from_utf16_lossy(&buf[..len]);
            return path.contains('\\').then_some(path);
        }
        if status != ntapi::STATUS_INFO_LENGTH_MISMATCH {
            return None;
        }
        buf.resize(info.ImageName.MaximumLength as usize / 2 + 1, 0);
    }
    None
}

#[derive(Default, Debug)]
pub struct FileInfo {
    pub description: Option<String>,
    pub company: Option<String>,
    pub version: Option<String>,
}

pub fn file_info(path: &str) -> FileInfo {
    let mut info = FileInfo::default();
    let path_w = wide(path);
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(path_w.as_ptr()), None) };
    if size == 0 {
        return info;
    }
    let mut data = vec![0u8; size as usize];
    if unsafe {
        GetFileVersionInfoW(
            PCWSTR(path_w.as_ptr()),
            None,
            size,
            data.as_mut_ptr().cast(),
        )
    }
    .is_err()
    {
        return info;
    }

    let query = |sub_block: &str| -> Option<(*const c_void, u32)> {
        let sub = wide(sub_block);
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        let ok = unsafe {
            VerQueryValueW(
                data.as_ptr().cast(),
                PCWSTR(sub.as_ptr()),
                &mut ptr,
                &mut len,
            )
        };
        (ok.as_bool() && !ptr.is_null() && len > 0).then_some((ptr.cast_const(), len))
    };

    let mut languages = Vec::new();
    if let Some((ptr, len)) = query(r"\VarFileInfo\Translation") {
        let pairs = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), len as usize / 2) };
        for pair in pairs.as_chunks::<2>().0 {
            languages.push(format!("{:04x}{:04x}", pair[0], pair[1]));
        }
    }
    languages.extend(["040904b0", "040904e4", "04090000"].map(String::from));

    let string = |name: &str| {
        languages.iter().find_map(|lang| {
            let (ptr, len) = query(&format!(r"\StringFileInfo\{lang}\{name}"))?;
            let chars = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), len as usize) };
            let s = from_wide(chars).trim().to_string();
            (!s.is_empty()).then_some(s)
        })
    };
    info.description = string("FileDescription");
    info.company = string("CompanyName");

    if let Some((ptr, len)) = query(r"\")
        && len as usize >= size_of::<VS_FIXEDFILEINFO>()
    {
        let fixed = unsafe { &*ptr.cast::<VS_FIXEDFILEINFO>() };
        info.version = Some(format!(
            "{}.{}.{}.{}",
            fixed.dwFileVersionMS >> 16,
            fixed.dwFileVersionMS & 0xFFFF,
            fixed.dwFileVersionLS >> 16,
            fixed.dwFileVersionLS & 0xFFFF
        ));
    } else {
        info.version = string("FileVersion");
    }
    info
}

/// The file's icon as RGBA8, roughly `size` pixels square.
pub fn extract_icon(path: &str, size: u32) -> Option<(u32, u32, Vec<u8>)> {
    let path_w = wide(path);
    let mut icon = HICON::default();
    let hr =
        unsafe { SHDefExtractIconW(PCWSTR(path_w.as_ptr()), 0, 0, Some(&mut icon), None, size) };
    if hr.is_err() || hr.0 != 0 || icon.is_invalid() {
        let mut sfi = SHFILEINFOW::default();
        let ok = unsafe {
            SHGetFileInfoW(
                PCWSTR(path_w.as_ptr()),
                FILE_ATTRIBUTE_NORMAL,
                Some(&mut sfi),
                size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON,
            )
        };
        if ok == 0 || sfi.hIcon.is_invalid() {
            return None;
        }
        icon = sfi.hIcon;
    }
    let pixels = icon_to_rgba(icon);
    unsafe {
        let _ = DestroyIcon(icon);
    }
    pixels
}

fn icon_to_rgba(icon: HICON) -> Option<(u32, u32, Vec<u8>)> {
    let mut info = ICONINFO::default();
    unsafe { GetIconInfo(icon, &mut info) }.ok()?;
    let result = (|| {
        if info.hbmColor.is_invalid() {
            return None;
        }
        let mut bm = BITMAP::default();
        let got = unsafe {
            GetObjectW(
                info.hbmColor.into(),
                size_of::<BITMAP>() as i32,
                Some((&raw mut bm).cast()),
            )
        };
        if got == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 || bm.bmWidth > 256 || bm.bmHeight > 256
        {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        let mut pixels = read_bitmap(info.hbmColor, w, h)?;
        if pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
            // No alpha channel: transparency lives in the AND mask.
            let mask = read_bitmap(info.hbmMask, w, h)?;
            for (p, m) in pixels
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(mask.as_chunks::<4>().0)
            {
                p[3] = if m[0] == 0 { 255 } else { 0 };
            }
        }
        for p in pixels.as_chunks_mut::<4>().0 {
            p.swap(0, 2); // BGRA -> RGBA
        }
        Some((w as u32, h as u32, pixels))
    })();
    unsafe {
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(info.hbmColor.into());
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(info.hbmMask.into());
        }
    }
    result
}

fn read_bitmap(bitmap: HBITMAP, w: i32, h: i32) -> Option<Vec<u8>> {
    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    unsafe {
        let dc = GetDC(None);
        let lines = GetDIBits(
            dc,
            bitmap,
            0,
            h as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, dc);
        (lines == h).then_some(pixels)
    }
}

/// `DOMAIN\user` for a binary SID.
pub fn lookup_account(sid: &[u8]) -> Option<String> {
    let psid = PSID(sid.as_ptr().cast_mut().cast());
    let mut name = [0u16; 256];
    let mut domain = [0u16; 256];
    let (mut name_len, mut domain_len) = (name.len() as u32, domain.len() as u32);
    let mut use_ = SID_NAME_USE::default();
    unsafe {
        LookupAccountSidW(
            PCWSTR::null(),
            psid,
            Some(PWSTR(name.as_mut_ptr())),
            &mut name_len,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_len,
            &mut use_,
        )
    }
    .ok()?;
    let name = from_wide(&name);
    let domain = from_wide(&domain);
    Some(if domain.is_empty() {
        name
    } else {
        format!(r"{domain}\{name}")
    })
}

pub fn sid_to_string(sid: &[u8]) -> String {
    let psid = PSID(sid.as_ptr().cast_mut().cast());
    let mut out = PWSTR::null();
    if unsafe { ConvertSidToStringSidW(psid, &mut out) }.is_err() || out.is_null() {
        return "?".into();
    }
    let s = unsafe { out.to_string() }.unwrap_or_default();
    unsafe {
        LocalFree(Some(HLOCAL(out.0.cast())));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel32_version_info() {
        let root = std::env::var("SystemRoot").unwrap();
        let info = file_info(&format!(r"{root}\System32\kernel32.dll"));
        assert!(info.description.is_some());
        assert!(info.company.as_deref().unwrap_or("").contains("Microsoft"));
        assert!(info.version.is_some());
    }

    #[test]
    fn cmd_icon_decodes() {
        let root = std::env::var("SystemRoot").unwrap();
        let (w, h, rgba) = extract_icon(&format!(r"{root}\System32\cmd.exe"), 32).expect("no icon");
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        assert!(
            rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 0),
            "icon is fully transparent"
        );
    }

    #[test]
    fn device_map_round_trips_system_drive() {
        let map = DeviceMap::build();
        let own = nt_image_path(std::process::id()).expect("no NT path for self");
        let dos = map.to_dos_path(&own).expect("could not translate");
        assert!(dos.chars().nth(1) == Some(':'), "{dos}");
    }

    #[test]
    fn system_sid_resolves() {
        // S-1-5-18, LocalSystem
        let sid = [1u8, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
        assert_eq!(sid_to_string(&sid), "S-1-5-18");
        assert!(
            lookup_account(&sid)
                .unwrap()
                .to_uppercase()
                .contains("SYSTEM")
        );
    }
}
