//! Monitors on Linux. The kernel's display connectors (/sys/class/drm) say
//! what is plugged in and what it is (its EDID: name, panel resolution,
//! refresh rates, HDR); the compositor says where each one sits and how it
//! is driven: KDE through kscreen-doctor, GNOME through Mutter's
//! DisplayConfig. Without either, the monitors are listed side by side at
//! their own resolution.

use std::collections::HashMap;

use super::*;

/// What a monitor's EDID says.
#[derive(Default, Debug, PartialEq)]
struct Edid {
    name: String,
    /// The first detailed timing: the panel's own mode.
    native: (u32, u32),
    native_hz: f64,
    /// The fastest refresh it declares (its range limits, or a timing).
    max_hz: u32,
    hdr: bool,
    bits_per_color: u32,
}

/// A detailed timing descriptor: (width, height, refresh).
fn timing(d: &[u8]) -> Option<(u32, u32, f64)> {
    let clock = u16::from_le_bytes([d[0], d[1]]) as f64 * 10_000.0;
    if clock == 0.0 {
        return None;
    }
    let h_active = d[2] as u32 | ((d[4] as u32 >> 4) << 8);
    let h_blank = d[3] as u32 | ((d[4] as u32 & 0xF) << 8);
    let v_active = d[5] as u32 | ((d[7] as u32 >> 4) << 8);
    let v_blank = d[6] as u32 | ((d[7] as u32 & 0xF) << 8);
    let total = ((h_active + h_blank) * (v_active + v_blank)) as f64;
    (total > 0.0).then(|| (h_active, v_active, clock / total))
}

fn parse_edid(d: &[u8]) -> Option<Edid> {
    if d.len() < 128 || d[..8] != [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0] {
        return None;
    }
    let mut e = Edid::default();
    // EDID 1.4, digital input: bits per primary colour.
    if d[20] & 0x80 != 0 {
        e.bits_per_color = match (d[20] >> 4) & 7 {
            1 => 6,
            2 => 8,
            3 => 10,
            4 => 12,
            5 => 14,
            6 => 16,
            _ => 0,
        };
    }
    let mut fastest = 0.0f64;
    for at in [54, 72, 90, 108] {
        let block = &d[at..at + 18];
        if let Some((w, h, hz)) = timing(block) {
            if e.native == (0, 0) {
                e.native = (w, h);
                e.native_hz = hz;
            }
            fastest = fastest.max(hz);
            continue;
        }
        match block[3] {
            // The monitor's name, ended by a newline.
            0xFC => {
                e.name = String::from_utf8_lossy(&block[5..18])
                    .split('\n')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
            }
            // Range limits: the highest vertical rate (offset by 255 when
            // flagged).
            0xFD => {
                let offset = if block[4] & 0x2 != 0 { 255 } else { 0 };
                fastest = fastest.max((block[6] as u32 + offset) as f64);
            }
            _ => {}
        }
    }
    // CTA-861 extensions: more timings, and HDR's static metadata block.
    for ext in d[128..].chunks_exact(128) {
        if ext[0] != 0x02 {
            continue;
        }
        let dtd_start = (ext[2] as usize).min(127);
        let mut i = 4;
        while i < dtd_start {
            let (tag, len) = (ext[i] >> 5, (ext[i] & 0x1F) as usize);
            let payload = &ext[(i + 1).min(127)..(i + 1 + len).min(127)];
            // Extended tag 6: HDR static metadata; bit 2 of its EOTFs is
            // SMPTE ST 2084 (PQ), what HDR10 needs.
            if tag == 7
                && payload.first() == Some(&6)
                && payload.get(1).is_some_and(|eotf| eotf & 0x4 != 0)
            {
                e.hdr = true;
            }
            i += 1 + len;
        }
        let mut at = dtd_start;
        while at + 18 <= 127 {
            match timing(&ext[at..at + 18]) {
                Some((_, _, hz)) => fastest = fastest.max(hz),
                None => break,
            }
            at += 18;
        }
    }
    e.max_hz = fastest.round() as u32;
    Some(e)
}

/// One connector the kernel lists as connected.
struct Connector {
    /// eDP-1, DP-4, HDMI-A-1.
    name: String,
    gpu: String,
    edid: Edid,
    enabled: bool,
}

fn connection(connector: &str) -> &'static str {
    let kind = connector
        .rsplit_once('-')
        .map_or(connector, |(kind, _)| kind);
    match kind {
        "eDP" | "LVDS" | "DSI" => "built in",
        "DP" => "DisplayPort",
        "HDMI-A" | "HDMI-B" => "HDMI",
        "DVI-D" | "DVI-I" | "DVI-A" => "DVI",
        "VGA" => "VGA",
        "Virtual" | "Writeback" => "virtual",
        _ => "other",
    }
}

fn connectors() -> Vec<Connector> {
    let Ok(dir) = std::fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    let mut names: Vec<String> = dir
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("card") && n.contains('-'))
        .collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|full| {
            let base = format!("/sys/class/drm/{full}");
            let read = |file: &str| std::fs::read_to_string(format!("{base}/{file}")).ok();
            if read("status")?.trim() != "connected" {
                return None;
            }
            let (card, name) = full.split_once('-')?;
            let pdev = std::fs::read_to_string(format!("/sys/class/drm/{card}/device/uevent"))
                .ok()
                .and_then(|u| {
                    u.lines()
                        .find_map(|l| l.strip_prefix("PCI_SLOT_NAME=").map(str::to_string))
                });
            Some(Connector {
                name: name.to_string(),
                gpu: pdev
                    .as_deref()
                    .and_then(crate::gpu::pci_name)
                    .unwrap_or_default(),
                edid: std::fs::read(format!("{base}/edid"))
                    .ok()
                    .and_then(|e| parse_edid(&e))
                    .unwrap_or_default(),
                enabled: read("enabled").is_none_or(|e| e.trim() == "enabled"),
            })
        })
        .collect()
}

/// A monitor as the compositor drives it.
struct Output {
    name: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    refresh_hz: f64,
    /// Every mode it offers: (width, height, refresh).
    modes: Vec<(u32, u32, f64)>,
    scale: f64,
    primary: bool,
    hdr_on: bool,
}

/// KDE's view, from kscreen-doctor's JSON (Plasma 5 and 6, Wayland and X11).
fn kscreen() -> Option<Vec<Output>> {
    let out = std::process::Command::new("kscreen-doctor")
        .arg("-j")
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let outputs = json.get("outputs")?.as_array()?;
    let mut list = Vec::new();
    for o in outputs {
        let on = |key: &str| o.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
        if !on("enabled") || !on("connected") {
            continue;
        }
        let size = |v: &serde_json::Value| -> (u32, u32) {
            let n = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0) as u32;
            (n("width"), n("height"))
        };
        let modes: Vec<(String, (u32, u32), f64)> = o
            .get("modes")
            .and_then(|m| m.as_array())
            .map(|modes| {
                modes
                    .iter()
                    .map(|m| {
                        (
                            m.get("id")
                                .and_then(|i| i.as_str())
                                .unwrap_or("")
                                .to_string(),
                            m.get("size").map_or((0, 0), size),
                            m.get("refreshRate").and_then(|r| r.as_f64()).unwrap_or(0.0),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let current = o
            .get("currentModeId")
            .and_then(|c| c.as_str())
            .unwrap_or("");
        let (width, height, refresh) = modes
            .iter()
            .find(|(id, _, _)| id == current)
            .map(|(_, (w, h), hz)| (*w, *h, *hz))
            .unwrap_or_else(|| {
                let (w, h) = o.get("size").map_or((0, 0), size);
                (w, h, 0.0)
            });
        let pos = |k: &str| {
            o.get("pos")
                .and_then(|p| p.get(k))
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32
        };
        list.push(Output {
            name: o
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string(),
            x: pos("x"),
            y: pos("y"),
            width,
            height,
            refresh_hz: refresh,
            modes: modes.iter().map(|(_, (w, h), hz)| (*w, *h, *hz)).collect(),
            scale: o.get("scale").and_then(|s| s.as_f64()).unwrap_or(1.0),
            // Priority 1 is the primary monitor.
            primary: o.get("priority").and_then(|p| p.as_u64()) == Some(1),
            hdr_on: on("hdr"),
        });
    }
    (!list.is_empty()).then_some(list)
}

/// A mode as Mutter lists it: id, width, height, refresh, preferred scale,
/// supported scales, properties ("is-current").
type MutterMode<'a> = (
    String,
    i32,
    i32,
    f64,
    f64,
    Vec<f64>,
    HashMap<String, zbus::zvariant::Value<'a>>,
);
type MutterMonitor<'a> = (
    (String, String, String, String),
    Vec<MutterMode<'a>>,
    HashMap<String, zbus::zvariant::Value<'a>>,
);
type MutterLogical<'a> = (
    i32,
    i32,
    f64,
    u32,
    bool,
    Vec<(String, String, String, String)>,
    HashMap<String, zbus::zvariant::Value<'a>>,
);

/// GNOME's view, from Mutter's DisplayConfig on the session bus.
fn mutter() -> Option<Vec<Output>> {
    let conn = zbus::blocking::Connection::session().ok()?;
    let reply = conn
        .call_method(
            Some("org.gnome.Mutter.DisplayConfig"),
            "/org/gnome/Mutter/DisplayConfig",
            Some("org.gnome.Mutter.DisplayConfig"),
            "GetCurrentState",
            &(),
        )
        .ok()?;
    let body = reply.body();
    let (_serial, monitors, logical, _props): (
        u32,
        Vec<MutterMonitor<'_>>,
        Vec<MutterLogical<'_>>,
        HashMap<String, zbus::zvariant::Value<'_>>,
    ) = body.deserialize().ok()?;
    let mut list = Vec::new();
    for (x, y, scale, _transform, primary, members, _) in logical {
        for (connector, ..) in members {
            let Some((_, modes, props)) = monitors.iter().find(|(id, ..)| id.0 == connector) else {
                continue;
            };
            let current = modes.iter().find(|m| {
                m.6.get("is-current")
                    .and_then(|v| bool::try_from(v).ok())
                    .unwrap_or(false)
            });
            let Some(current) = current else {
                continue;
            };
            list.push(Output {
                name: connector.clone(),
                x,
                y,
                width: current.1 as u32,
                height: current.2 as u32,
                refresh_hz: current.3,
                modes: modes
                    .iter()
                    .map(|m| (m.1 as u32, m.2 as u32, m.3))
                    .collect(),
                scale,
                primary,
                hdr_on: props
                    .get("color-mode")
                    .and_then(|v| u32::try_from(v).ok())
                    .is_some_and(|mode| mode == 1),
            });
        }
    }
    (!list.is_empty()).then_some(list)
}

pub fn displays() -> Vec<Display> {
    let connectors = connectors();
    let outputs = kscreen().or_else(mutter);
    let Some(outputs) = outputs else {
        // Nobody to ask about the layout: each at its own resolution, side
        // by side in the kernel's order.
        let mut x = 0;
        return connectors
            .iter()
            .filter(|c| c.enabled)
            .enumerate()
            .map(|(i, c)| {
                let (width, height) = c.edid.native;
                let d = Display {
                    device: c.name.clone(),
                    name: c.edid.name.clone(),
                    gpu: c.gpu.clone(),
                    connection: connection(&c.name),
                    primary: i == 0,
                    x,
                    y: 0,
                    width,
                    height,
                    refresh_hz: c.edid.native_hz,
                    max_hz_here: c.edid.max_hz,
                    max_hz: c.edid.max_hz,
                    native: c.edid.native,
                    hdr_supported: c.edid.hdr,
                    hdr_on: false,
                    bits_per_color: c.edid.bits_per_color,
                    scale_pct: 100,
                };
                x += width as i32;
                d
            })
            .collect();
    };
    let mut list: Vec<Display> = outputs
        .into_iter()
        .map(|o| {
            let c = connectors.iter().find(|c| c.name == o.name);
            let edid = c.map(|c| &c.edid);
            let here = o
                .modes
                .iter()
                .filter(|(w, h, _)| (*w, *h) == (o.width, o.height))
                .map(|(_, _, hz)| hz.round() as u32)
                .max()
                .unwrap_or(0);
            let any = o
                .modes
                .iter()
                .map(|(_, _, hz)| hz.round() as u32)
                .max()
                .unwrap_or(0);
            let largest = o
                .modes
                .iter()
                .map(|(w, h, _)| (*w, *h))
                .max_by_key(|(w, h)| w * h)
                .unwrap_or((o.width, o.height));
            Display {
                connection: connection(&o.name),
                device: o.name,
                name: edid.map(|e| e.name.clone()).unwrap_or_default(),
                gpu: c.map(|c| c.gpu.clone()).unwrap_or_default(),
                primary: o.primary,
                x: o.x,
                y: o.y,
                width: o.width,
                height: o.height,
                refresh_hz: o.refresh_hz,
                max_hz_here: here.max(o.refresh_hz.round() as u32),
                // What the compositor offers; the EDID's range limits only
                // when it lists no modes (a monitor may accept rates no
                // mode uses).
                max_hz: if any > 0 {
                    any
                } else {
                    edid.map_or(0, |e| e.max_hz)
                },
                native: edid
                    .map(|e| e.native)
                    .filter(|n| *n != (0, 0))
                    .unwrap_or(largest),
                hdr_supported: edid.is_some_and(|e| e.hdr),
                hdr_on: o.hdr_on,
                bits_per_color: edid.map_or(0, |e| e.bits_per_color),
                scale_pct: (o.scale * 100.0).round() as u32,
            }
        })
        .collect();
    // The primary one first, then left to right.
    list.sort_by_key(|d| (!d.primary, d.x, d.y));
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2560×1440 monitor named T27h-30 with range limits of 48-75 Hz and
    /// a CTA extension carrying no HDR block (a Lenovo's, abridged).
    fn edid() -> Vec<u8> {
        let mut d = vec![0u8; 256];
        d[..8].copy_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
        d[20] = 0xA5;
        // 241.5 MHz, 2560+160 × 1440+41: 59.95 Hz.
        d[54..72].copy_from_slice(&[
            0x56, 0x5E, 0x00, 0xA0, 0xA0, 0xA0, 0x29, 0x50, 0x30, 0x20, 0x35, 0x00, 0x55, 0x50,
            0x21, 0x00, 0x00, 0x1A,
        ]);
        d[72..90].copy_from_slice(&[
            0, 0, 0, 0xFD, 0, 0x30, 0x4B, 0x1E, 0x72, 0x1E, 0x01, 0x0A, 0x20, 0x20, 0x20, 0x20,
            0x20, 0x20,
        ]);
        let mut name = [0x20u8; 18];
        name[..5].copy_from_slice(&[0, 0, 0, 0xFC, 0]);
        name[5..13].copy_from_slice(b"T27h-30\n");
        d[90..108].copy_from_slice(&name);
        d[126] = 1;
        d[128] = 0x02;
        d[130] = 4;
        d
    }

    #[test]
    fn reads_an_edid() {
        let e = parse_edid(&edid()).unwrap();
        assert_eq!(e.name, "T27h-30");
        assert_eq!(e.native, (2560, 1440));
        assert!((e.native_hz - 59.95).abs() < 0.01, "{}", e.native_hz);
        assert_eq!(e.max_hz, 75);
        assert_eq!(e.bits_per_color, 8);
        assert!(!e.hdr);
    }

    #[test]
    fn finds_hdr_in_the_cta_block() {
        let mut d = edid();
        // Extended tag 6 (HDR static metadata), EOTFs: SDR, PQ.
        d[130] = 8;
        d[132..136].copy_from_slice(&[0xE3, 0x06, 0x05, 0x01]);
        assert!(parse_edid(&d).unwrap().hdr);
    }

    #[test]
    fn names_connections() {
        assert_eq!(connection("eDP-1"), "built in");
        assert_eq!(connection("DP-4"), "DisplayPort");
        assert_eq!(connection("HDMI-A-2"), "HDMI");
    }
}
