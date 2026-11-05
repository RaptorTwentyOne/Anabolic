// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! How the drives are holding up, as they report it themselves: an NVMe
//! drive's health log, a SATA drive's SMART attributes. How much of its
//! rated wear is used, its temperature, hours, power cycles, unexpected
//! power losses, what it wrote, and the errors that say it is failing,
//! with a verdict in words. A drive that sleeps is not woken to ask.

#[cfg_attr(windows, path = "windows/health.rs")]
#[cfg_attr(target_os = "linux", path = "linux/health.rs")]
mod imp;
pub use imp::read;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    /// Nothing was read: the drive does not say, or sleeps.
    #[default]
    Unknown,
    Good,
    /// Worth watching: early signs, or most of its wear used.
    Watch,
    /// Failing, or about to: back up what is on it.
    Bad,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Unknown => "Not known",
            Verdict::Good => "Good",
            Verdict::Watch => "Watch it",
            Verdict::Bad => "Failing",
        }
    }
}

/// One SMART attribute of a SATA drive.
#[derive(Clone, Copy, Debug, Default)]
pub struct Attribute {
    pub id: u8,
    /// The drive's own score, usually from 100 or 200 down; 0 when it
    /// keeps none.
    pub current: u8,
    pub worst: u8,
    /// Where the drive counts it as failed; 0 when not known.
    pub threshold: u8,
    pub raw: u64,
}

impl Attribute {
    /// What the common ones are called.
    pub fn name(&self) -> &'static str {
        match self.id {
            1 => "Read errors",
            3 => "Spin-up time",
            4 => "Starts and stops",
            5 => "Reallocated sectors",
            7 => "Seek errors",
            9 => "Hours powered on",
            10 => "Spin-up retries",
            12 => "Power cycles",
            169 => "Life left",
            170 => "Spare blocks",
            171 => "Program failures",
            172 => "Erase failures",
            173 => "Wear leveling",
            174 => "Unexpected power losses",
            177 => "Wear leveling",
            179 => "Spare blocks used",
            181 => "Program failures",
            182 => "Erase failures",
            183 => "Runtime bad blocks",
            184 => "End-to-end errors",
            187 => "Reported uncorrectable",
            188 => "Command timeouts",
            190 => "Airflow temperature",
            191 => "Shocks",
            192 => "Power-off retracts",
            193 => "Head loads",
            194 => "Temperature",
            195 => "Corrected by ECC",
            196 => "Reallocation events",
            197 => "Pending sectors",
            198 => "Uncorrectable sectors",
            199 => "Cable errors (CRC)",
            200 => "Write errors",
            202 => "Life used",
            231 => "Life left",
            232 => "Spare left",
            233 => "Media wearout",
            235 => "Power losses recovered",
            241 => "Written",
            242 => "Read",
            _ => "",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Health {
    /// The disk's number, as `DiskStats::number`.
    pub number: u32,
    pub model: String,
    /// "NVMe", "SATA".
    pub protocol: &'static str,
    pub verdict: Verdict,
    /// Why, in words: each sign that set the verdict.
    pub reasons: Vec<String>,
    /// The share of its rated wear used, %; may pass 100.
    pub wear_used: Option<u32>,
    pub temperature: Option<i16>,
    pub power_on_hours: Option<u64>,
    pub power_cycles: Option<u64>,
    /// Power lost without the drive being told first.
    pub unsafe_shutdowns: Option<u64>,
    pub written: Option<u64>,
    pub read: Option<u64>,
    /// NVMe: errors the drive could not correct; SATA: sectors moved
    /// aside, waiting to be, and that could not be read.
    pub media_errors: Option<u64>,
    pub reallocated: Option<u64>,
    pub pending: Option<u64>,
    pub uncorrectable: Option<u64>,
    /// NVMe spare blocks left and where the drive warns, %.
    pub spare: Option<(u8, u8)>,
    /// The drive itself says it is failing (SMART's own verdict, or an
    /// NVMe critical warning).
    pub drive_warns: bool,
    /// Every SMART attribute, for a SATA drive.
    pub attributes: Vec<Attribute>,
    /// Why nothing could be read.
    pub note: Option<String>,
}

impl Health {
    /// Sets the verdict and its reasons from what was read.
    pub(crate) fn judge(&mut self) {
        let mut bad = Vec::new();
        let mut watch = Vec::new();
        if self.drive_warns {
            bad.push("The drive itself reports that it is failing".to_string());
        }
        if let Some((left, warn)) = self.spare
            && warn > 0
            && left < warn
        {
            bad.push(format!(
                "Spare blocks are below what it needs ({left} % left)"
            ));
        }
        match self.wear_used {
            Some(w) if w >= 100 => bad.push(format!("{w} % of its rated wear is used")),
            Some(w) if w >= 80 => watch.push(format!("{w} % of its rated wear is used")),
            _ => {}
        }
        if let Some(n) = self.pending.filter(|&n| n > 0) {
            bad.push(if n == 1 {
                "1 sector is waiting to be moved aside".to_string()
            } else {
                format!("{n} sectors are waiting to be moved aside")
            });
        }
        if let Some(n) = self.uncorrectable.filter(|&n| n > 0) {
            bad.push(if n == 1 {
                "1 sector could not be read".to_string()
            } else {
                format!("{n} sectors could not be read")
            });
        }
        if let Some(n) = self.reallocated.filter(|&n| n > 0) {
            watch.push(if n == 1 {
                "1 sector was moved aside".to_string()
            } else {
                format!("{n} sectors were moved aside")
            });
        }
        if self.protocol == "NVMe"
            && let Some(n) = self.media_errors.filter(|&n| n > 0)
        {
            watch.push(if n == 1 {
                "1 error it could not correct".to_string()
            } else {
                format!("{n} errors it could not correct")
            });
        }
        // A SATA attribute at or below the drive's own limit.
        for a in &self.attributes {
            if a.threshold > 0 && a.current > 0 && a.current <= a.threshold {
                let name = match a.name() {
                    "" => format!("attribute {}", a.id),
                    n => n.to_lowercase(),
                };
                bad.push(format!("The drive's {name} is past its limit"));
            }
        }
        let read_any = self.wear_used.is_some()
            || self.temperature.is_some()
            || self.power_on_hours.is_some()
            || !self.attributes.is_empty();
        self.verdict = if !bad.is_empty() {
            Verdict::Bad
        } else if !watch.is_empty() {
            Verdict::Watch
        } else if read_any || self.drive_warns {
            Verdict::Good
        } else {
            Verdict::Unknown
        };
        bad.dedup();
        self.reasons = bad;
        self.reasons.extend(watch);
    }

    /// Fills the figures from a SATA drive's attributes.
    pub(crate) fn take_attributes(&mut self, attributes: Vec<Attribute>) {
        let get = |id: u8| attributes.iter().find(|a| a.id == id);
        let raw = |id: u8| get(id).map(|a| a.raw);
        // Hours, powers and temperatures keep other things in the upper
        // bytes on some drives.
        self.power_on_hours = raw(9).map(|r| r & 0xFFFF_FFFF);
        self.power_cycles = raw(12);
        self.unsafe_shutdowns = raw(174).or(raw(235)).or(raw(192));
        self.temperature = get(194)
            .or(get(190))
            .map(|a| (a.raw & 0xFF) as i16)
            .filter(|t| (1..100).contains(t));
        self.reallocated = raw(5);
        self.pending = raw(197);
        self.uncorrectable = raw(198);
        // What is left of its life, as the drive scores it from 100.
        self.wear_used = [231u8, 177, 233, 169]
            .iter()
            .find_map(|&id| get(id).filter(|a| a.current > 0 && a.current <= 100))
            .map(|a| 100 - u32::from(a.current));
        // Most drives count 512-byte sectors.
        self.written = raw(241).map(|r| r.saturating_mul(512));
        self.read = raw(242).map(|r| r.saturating_mul(512));
        self.attributes = attributes;
    }
}

/// The attribute table of a SATA drive's SMART data: 30 entries of 12
/// bytes after a 2-byte version.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn attributes_of(data: &[u8], thresholds: Option<&[u8]>) -> Vec<Attribute> {
    let mut out = Vec::new();
    for i in 0..30 {
        let at = 2 + i * 12;
        let Some(e) = data.get(at..at + 12) else {
            break;
        };
        if e[0] == 0 {
            continue;
        }
        let mut raw = [0u8; 8];
        raw[..6].copy_from_slice(&e[5..11]);
        let threshold = thresholds
            .and_then(|t| {
                (0..30)
                    .map(|j| 2 + j * 12)
                    .filter_map(|o| t.get(o..o + 2))
                    .find(|x| x[0] == e[0])
                    .map(|x| x[1])
            })
            .unwrap_or(0);
        out.push(Attribute {
            id: e[0],
            current: e[3],
            worst: e[4],
            threshold,
            raw: u64::from_le_bytes(raw),
        });
    }
    out
}

/// An NVMe drive's health log page (02h), 512 bytes.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn from_nvme_log(h: &mut Health, log: &[u8]) {
    if log.len() < 200 {
        return;
    }
    let u128_at = |at: usize| {
        let mut b = [0u8; 16];
        b.copy_from_slice(&log[at..at + 16]);
        u128::from_le_bytes(b)
    };
    let count = |at: usize| u64::try_from(u128_at(at)).unwrap_or(u64::MAX);
    let warning = log[0];
    // Spare below its limit, reliability degraded, read-only, backup
    // failed: the drive warns. Temperature alone is a moment's heat.
    h.drive_warns = warning & 0b0011_1101 != 0;
    let kelvin = u16::from_le_bytes([log[1], log[2]]);
    h.temperature = (kelvin > 200).then(|| (i32::from(kelvin) - 273) as i16);
    h.spare = Some((log[3], log[4]));
    h.wear_used = Some(u32::from(log[5]));
    // Data units are thousands of 512-byte blocks.
    h.read = Some(count(32).saturating_mul(512_000));
    h.written = Some(count(48).saturating_mul(512_000));
    h.power_cycles = Some(count(112));
    h.power_on_hours = Some(count(128));
    h.unsafe_shutdowns = Some(count(144));
    h.media_errors = Some(count(160));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u8, current: u8, raw: u64) -> [u8; 12] {
        let mut e = [0u8; 12];
        e[0] = id;
        e[3] = current;
        e[4] = current;
        e[5..11].copy_from_slice(&raw.to_le_bytes()[..6]);
        e
    }

    #[test]
    fn reads_sata_attributes() {
        let mut data = vec![0u8; 512];
        for (i, e) in [
            entry(5, 100, 0),
            entry(9, 99, 33_309),
            entry(12, 99, 1_200),
            entry(177, 93, 120),
            entry(190, 70, 30),
            entry(197, 100, 2),
            entry(241, 99, 1_000_000),
        ]
        .iter()
        .enumerate()
        {
            data[2 + i * 12..2 + i * 12 + 12].copy_from_slice(e);
        }
        let mut thresholds = vec![0u8; 512];
        thresholds[2] = 5;
        thresholds[3] = 10;
        let attrs = attributes_of(&data, Some(&thresholds));
        assert_eq!(attrs.len(), 7);
        assert_eq!(attrs[0].threshold, 10);
        let mut h = Health {
            protocol: "SATA",
            ..Health::default()
        };
        h.take_attributes(attrs);
        assert_eq!(h.power_on_hours, Some(33_309));
        assert_eq!(
            h.wear_used,
            Some(7),
            "Samsung's wear leveling scores life left"
        );
        assert_eq!(h.temperature, Some(30));
        assert_eq!(h.written, Some(512_000_000));
        h.judge();
        assert_eq!(h.verdict, Verdict::Bad, "pending sectors");
        assert!(h.reasons[0].contains("2 sectors"));
    }

    #[test]
    fn reads_an_nvme_health_log() {
        let mut log = vec![0u8; 512];
        log[1..3].copy_from_slice(&314u16.to_le_bytes());
        log[3] = 100;
        log[4] = 10;
        log[5] = 3;
        log[48..56].copy_from_slice(&20_000_000u64.to_le_bytes());
        log[112] = 42;
        log[128..136].copy_from_slice(&5_000u64.to_le_bytes());
        log[144] = 7;
        let mut h = Health {
            protocol: "NVMe",
            ..Health::default()
        };
        from_nvme_log(&mut h, &log);
        assert_eq!(h.temperature, Some(41));
        assert_eq!(h.wear_used, Some(3));
        assert_eq!(h.written, Some(10_240_000_000_000));
        assert_eq!(h.power_on_hours, Some(5_000));
        assert_eq!(h.unsafe_shutdowns, Some(7));
        h.judge();
        assert_eq!(h.verdict, Verdict::Good);
        log[0] = 0b100;
        from_nvme_log(&mut h, &log);
        h.judge();
        assert_eq!(h.verdict, Verdict::Bad);
    }
}
