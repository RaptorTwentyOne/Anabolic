// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Wi-Fi, in the manner of WifiInfoView and inSSIDer: the networks around,
//! each access point with its band, channel, signal, security and Wi-Fi
//! generation, the one this PC is on with its link speed, and how crowded
//! each channel is, with the least crowded one to move the router to. It
//! only listens: the adapter's own scan, nothing sent.

#[cfg_attr(windows, path = "windows/wifi.rs")]
#[cfg_attr(target_os = "linux", path = "linux/wifi.rs")]
mod imp;
pub use imp::read;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Band {
    Ghz24,
    Ghz5,
    Ghz6,
}

impl Band {
    pub fn label(self) -> &'static str {
        match self {
            Band::Ghz24 => "2.4 GHz",
            Band::Ghz5 => "5 GHz",
            Band::Ghz6 => "6 GHz",
        }
    }

    /// The channels worth moving a router to: the three that do not
    /// overlap on 2.4 GHz, the ones without radar checks on 5 GHz.
    pub fn good_channels(self) -> &'static [u32] {
        match self {
            Band::Ghz24 => &[1, 6, 11],
            Band::Ghz5 => &[36, 40, 44, 48, 149, 153, 157, 161, 165],
            Band::Ghz6 => &[5, 21, 37, 53, 69, 85, 101, 117],
        }
    }
}

/// The band and channel of a centre frequency in MHz.
pub fn channel(mhz: u32) -> Option<(Band, u32)> {
    match mhz {
        2484 => Some((Band::Ghz24, 14)),
        2412..=2472 => Some((Band::Ghz24, (mhz - 2407) / 5)),
        5150..=5895 => Some((Band::Ghz5, (mhz - 5000) / 5)),
        5925..=7125 => Some((Band::Ghz6, (mhz - 5950) / 5)),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccessPoint {
    /// The network's name; empty for a hidden one.
    pub ssid: String,
    pub bssid: String,
    pub band: Option<Band>,
    pub channel: u32,
    pub dbm: i32,
    /// 0 to 100.
    pub quality: u32,
    /// "Wi-Fi 6", "Wi-Fi 7".
    pub generation: String,
    pub security: String,
    /// The one this PC is on.
    pub connected: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Link {
    pub ssid: String,
    pub bssid: String,
    pub band: Option<Band>,
    pub channel: u32,
    pub quality: u32,
    pub rx_mbps: u32,
    pub tx_mbps: u32,
    pub generation: String,
}

#[derive(Clone, Debug, Default)]
pub struct Wifi {
    pub adapter: String,
    pub link: Option<Link>,
    /// Strongest first.
    pub points: Vec<AccessPoint>,
    /// Why the list could be short: location access off, no adapter.
    pub note: Option<String>,
}

/// How much other networks crowd a channel: each one that shares or
/// overlaps it, weighted by how loud it is here.
pub fn crowding(points: &[AccessPoint], band: Band, ch: u32, skip_bssid: &str) -> f32 {
    points
        .iter()
        .filter(|p| p.band == Some(band) && p.bssid != skip_bssid)
        .filter(|p| {
            let gap = p.channel.abs_diff(ch);
            // 2.4 GHz channels overlap four either side; elsewhere a
            // 20 MHz neighbour does not.
            if band == Band::Ghz24 {
                gap < 5
            } else {
                gap == 0
            }
        })
        .map(|p| ((p.dbm + 100).clamp(0, 70) as f32 / 70.0).max(0.05))
        .sum()
}

/// The least crowded channel of a band, and how crowded it is.
pub fn quietest(points: &[AccessPoint], band: Band, skip_bssid: &str) -> (u32, f32) {
    band.good_channels()
        .iter()
        .map(|&c| (c, crowding(points, band, c, skip_bssid)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap_or((0, 0.0))
}

/// In words: how the network in use fares, and where to move it.
pub fn advice(w: &Wifi) -> Option<String> {
    let link = w.link.as_ref()?;
    let band = link.band?;
    let here = crowding(&w.points, band, link.channel, &link.bssid);
    let (best, quiet) = quietest(&w.points, band, &link.bssid);
    let mut out = format!(
        "{} is on channel {} ({}), signal {} %",
        link.ssid,
        link.channel,
        band.label(),
        link.quality
    );
    if link.quality < 50 {
        out.push_str(": weak; closer to the router, or fewer walls, helps most");
    }
    if best != link.channel && here > quiet + 0.5 {
        out.push_str(&format!(
            ". Channel {best} is less crowded here: the router's own settings move it."
        ));
    } else {
        out.push_str(". Its channel is about as quiet as this band gets here.");
    }
    if band == Band::Ghz24
        && w.points
            .iter()
            .any(|p| p.ssid == link.ssid && p.band != Some(Band::Ghz24))
    {
        out.push_str(" The same network is on a faster band too.");
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ap(bssid: &str, band: Band, ch: u32, dbm: i32) -> AccessPoint {
        AccessPoint {
            bssid: bssid.into(),
            band: Some(band),
            channel: ch,
            dbm,
            ..AccessPoint::default()
        }
    }

    #[test]
    fn finds_channels() {
        assert_eq!(channel(2412), Some((Band::Ghz24, 1)));
        assert_eq!(channel(2437), Some((Band::Ghz24, 6)));
        assert_eq!(channel(5180), Some((Band::Ghz5, 36)));
        assert_eq!(channel(5955), Some((Band::Ghz6, 1)));
        assert_eq!(channel(900), None);
    }

    #[test]
    fn picks_the_quiet_channel() {
        let points = [
            ap("a", Band::Ghz24, 6, -40),
            ap("b", Band::Ghz24, 7, -50),
            ap("c", Band::Ghz24, 11, -80),
            ap("me", Band::Ghz24, 6, -30),
        ];
        let (best, _) = quietest(&points, Band::Ghz24, "me");
        assert_eq!(best, 1);
        assert!(crowding(&points, Band::Ghz24, 6, "me") > crowding(&points, Band::Ghz24, 11, "me"));
    }
}
