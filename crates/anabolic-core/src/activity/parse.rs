//! Reading the kernel's event payloads: processes, images, registry keys,
//! connections, and file create options in words.

use super::*;

/// NUL-terminated UTF-16 at `at`.
pub(super) fn wide_at(d: &[u8], at: usize) -> Option<String> {
    let units: Vec<u16> = d
        .get(at..)?
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// A process start or end: pid, parent, exit status, image, command line.
#[derive(Debug, PartialEq)]
pub struct ProcessFacts {
    pub pid: u32,
    pub parent: u32,
    pub exit: u32,
    pub image: String,
    pub command_line: String,
}

/// Process_TypeGroup1: UniqueProcessKey, ProcessId, ParentId, SessionId,
/// ExitStatus, DirectoryTableBase, Flags, UserSID, ImageFileName (ANSI),
/// CommandLine (UTF-16), ...
pub fn parse_process(d: &[u8]) -> Option<ProcessFacts> {
    let pid = u32_at(d, 8)?;
    let parent = u32_at(d, 12)?;
    let exit = u32_at(d, 20)?;
    let sid = 36;
    let image_at = if u32_at(d, sid)? == 0 {
        sid + 4
    } else {
        let sub_authorities = *d.get(sid + 16 + 1)? as usize;
        sid + 16 + 8 + 4 * sub_authorities
    };
    let bytes = d.get(image_at..)?;
    let end = bytes.iter().position(|&b| b == 0)?;
    let image = String::from_utf8_lossy(&bytes[..end]).into_owned();
    let command_line = wide_at(d, image_at + end + 1).unwrap_or_default();
    Some(ProcessFacts {
        pid,
        parent,
        exit,
        image,
        command_line: command_line.trim().to_string(),
    })
}

/// Image_Load: ImageBase, ImageSize, ProcessId, CheckSum, TimeDateStamp,
/// Reserved0, DefaultBase, Reserved1..4, FileName.
pub fn parse_image(d: &[u8]) -> Option<(u32, String)> {
    Some((u32_at(d, 16)?, wide_at(d, 56)?))
}

/// Registry_TypeGroup1: (status, index, key handle, key name).
pub fn parse_registry(d: &[u8]) -> Option<(u32, u32, u64, String)> {
    Some((
        u32_at(d, 8)?,
        u32_at(d, 12)?,
        u64_at(d, 16)?,
        wide_at(d, 24)?,
    ))
}

/// A send, receive, connect, accept or disconnect: pid, bytes, remote,
/// remote port, local port. IPv4 and IPv6 layouts differ in address width;
/// ports are in network byte order.
pub fn parse_net(d: &[u8], v6: bool) -> Option<(u32, u32, IpAddr, u16, u16)> {
    let pid = u32_at(d, 0)?;
    let size = u32_at(d, 4)?;
    let be = |at: usize| Some(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?));
    if v6 {
        let addr: [u8; 16] = d.get(8..24)?.try_into().ok()?;
        let v6 = Ipv6Addr::from(addr);
        let ip = v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4);
        Some((pid, size, ip, be(40)?, be(42)?))
    } else {
        let addr: [u8; 4] = d.get(8..12)?.try_into().ok()?;
        Some((
            pid,
            size,
            IpAddr::V4(Ipv4Addr::from(addr)),
            be(16)?,
            be(18)?,
        ))
    }
}

/// The disposition a create asked for, in words; options it is worth
/// knowing about after it.
pub fn create_detail(options: u32) -> String {
    let disposition = match options >> 24 {
        0 => "supersede",
        1 => "open",
        2 => "create new",
        3 => "open or create",
        4 => "overwrite",
        5 => "overwrite or create",
        _ => "?",
    };
    let mut detail = disposition.to_string();
    if options & DIRECTORY_FILE != 0 {
        detail.push_str(", folder");
    }
    if options & DELETE_ON_CLOSE != 0 {
        detail.push_str(", delete on close");
    }
    detail
}

/// What a finished create did (IO_STATUS_BLOCK.Information).
pub(super) fn create_outcome(info: u64) -> Option<&'static str> {
    Some(match info {
        0 => "superseded",
        1 => "opened",
        2 => "created",
        3 => "overwritten",
        4 => "exists",
        5 => "does not exist",
        _ => return None,
    })
}

/// A kernel registry path the way regedit shows it: HKLM, HKCU, HKU.
pub fn friendly_key(name: &str, user_sid: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let strip = |prefix: &str| -> Option<&str> {
        lower
            .starts_with(prefix)
            .then(|| &name[prefix.len()..])
            .filter(|rest| rest.is_empty() || rest.starts_with('\\'))
    };
    if let Some(rest) = strip("\\registry\\machine") {
        return format!("HKLM{rest}");
    }
    if !user_sid.is_empty() {
        let classes = format!(
            "\\registry\\user\\{}_classes",
            user_sid.to_ascii_lowercase()
        );
        if let Some(rest) = strip(&classes) {
            return format!("HKCU\\Software\\Classes{rest}");
        }
        let user = format!("\\registry\\user\\{}", user_sid.to_ascii_lowercase());
        if let Some(rest) = strip(&user) {
            return format!("HKCU{rest}");
        }
    }
    if let Some(rest) = strip("\\registry\\user") {
        return format!("HKU{rest}");
    }
    name.to_string()
}
