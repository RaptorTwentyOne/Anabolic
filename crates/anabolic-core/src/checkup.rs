// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Checkup: what costs speed without saying so, the states and settings
//! power users look up one by one in CPU-Z, GPU-Z, HWiNFO and Windows'
//! settings. Memory below the speed it is made for, or all on one channel;
//! a graphics card or drive on fewer PCIe lanes than it has; Resizable BAR;
//! background recording; a monitor below its refresh rate; a cable that
//! holds a gigabit port to 100 Mbps; and more. Each with what it is now,
//! what it costs and where to change it. It only reads: the change is left
//! to the setting's own place.

use std::time::{Duration, Instant};

use crate::displays::Display;
use crate::hwinfo::{MemoryInfo, PageFile};

#[cfg_attr(windows, path = "windows/checkup.rs")]
#[cfg_attr(target_os = "linux", path = "linux/checkup.rs")]
mod imp;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Verdict {
    /// Costs speed now, and can be changed.
    Change,
    /// May cost something, depending on use.
    Consider,
    /// A setting with a trade either way: what it is and what it means.
    Info,
    /// As it should be.
    Good,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Change => "Change it",
            Verdict::Consider => "Worth a look",
            Verdict::Info => "Good to know",
            Verdict::Good => "Good",
        }
    }

    /// For tools: change, consider, info, good.
    pub fn key(self) -> &'static str {
        match self {
            Verdict::Change => "change",
            Verdict::Consider => "consider",
            Verdict::Info => "info",
            Verdict::Good => "good",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Area {
    Memory,
    Graphics,
    Displays,
    Storage,
    Power,
    Network,
    System,
    Input,
}

impl Area {
    pub fn label(self) -> &'static str {
        match self {
            Area::Memory => "Memory",
            Area::Graphics => "Graphics",
            Area::Displays => "Displays",
            Area::Storage => "Storage",
            Area::Power => "Power",
            Area::Network => "Network",
            Area::System => "System",
            Area::Input => "Mouse",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fix {
    /// A settings page, opened by its address (ms-settings:…).
    Open { label: String, uri: String },
    /// In the firmware's setup (BIOS or UEFI), entered as the PC starts.
    Firmware(String),
    /// By hand, in words.
    Hand(String),
}

impl Fix {
    pub fn open(label: &str, uri: &str) -> Fix {
        Fix::Open {
            label: label.into(),
            uri: uri.into(),
        }
    }

    /// In words, for a tool or a copy.
    pub fn words(&self) -> String {
        match self {
            Fix::Open { label, uri } => format!("{label} ({uri})"),
            Fix::Firmware(t) => format!("In the firmware setup: {t}"),
            Fix::Hand(t) => t.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Check {
    /// A stable name for tools: "memory-speed", "pcie-gpu-0".
    pub key: String,
    pub area: Area,
    pub title: String,
    pub verdict: Verdict,
    /// What it is now, in a few words.
    pub now: String,
    /// What it costs or means; empty when there is nothing to add.
    pub why: String,
    pub fix: Option<Fix>,
}

impl Check {
    pub fn new(
        key: impl Into<String>,
        area: Area,
        title: impl Into<String>,
        verdict: Verdict,
        now: impl Into<String>,
        why: impl Into<String>,
    ) -> Check {
        Check {
            key: key.into(),
            area,
            title: title.into(),
            verdict,
            now: now.into(),
            why: why.into(),
            fix: None,
        }
    }

    pub fn fix(mut self, fix: Fix) -> Check {
        self.fix = Some(fix);
        self
    }
}

#[derive(Debug, Default)]
pub struct Checkup {
    pub checks: Vec<Check>,
    pub took: Duration,
}

impl Checkup {
    pub fn count(&self, verdict: Verdict) -> usize {
        self.checks.iter().filter(|c| c.verdict == verdict).count()
    }
}

/// Every check, what to change first.
pub fn read() -> Checkup {
    let started = Instant::now();
    let hw = crate::hwinfo::hardware_info();
    let mut checks = memory(&hw.memory);
    checks.extend(page_file(&crate::hwinfo::page_files()));
    checks.extend(displays(&crate::displays::displays()));
    checks.extend(imp::checks());
    checks.sort_by_key(|c| (c.verdict, c.area));
    Checkup {
        checks,
        took: started.elapsed(),
    }
}

/// DDR4 and DDR5 speeds that kits are sold at, MT/s.
const SPEEDS: [u32; 38] = [
    2133, 2400, 2666, 2800, 2933, 3000, 3200, 3333, 3466, 3600, 3733, 3800, 3866, 4000, 4133, 4266,
    4400, 4600, 4800, 5000, 5200, 5400, 5600, 6000, 6200, 6400, 6600, 6800, 7000, 7200, 7400, 7600,
    7800, 8000, 8200, 8400, 8800, 9600,
];

/// The speed a memory module is sold at, from its part number: makers put
/// it there (Corsair CMT64GX5M2B6000Z30, G.Skill F5-6000J3038F16G,
/// Kingston KF560C36BBE-16, Crucial BL16G36C16U4B, Patriot PVV532G600C36K).
/// The SMBIOS tables give only the standard (JEDEC) speed, which an EXPO
/// or XMP kit runs at until its profile is turned on.
pub fn rated_speed(part: &str) -> Option<u32> {
    let p = part.trim().to_uppercase();
    let known = |n: u32| SPEEDS.contains(&n).then_some(n);
    // Kingston FURY: KF5 + two digits of hundreds + C.
    if let Some(rest) = p.strip_prefix("KF5").or_else(|| p.strip_prefix("KF4"))
        && rest.len() > 2
        && rest.as_bytes()[2] == b'C'
        && let Ok(n) = rest[..2].parse::<u32>()
    {
        return known(n * 100);
    }
    let digits: Vec<(usize, &str)> = runs(&p);
    // Four digits in a row that are a sold speed: 6000, 3600.
    let four = digits
        .iter()
        .flat_map(|(_, run)| (0..run.len().saturating_sub(3)).map(move |i| &run[i..i + 4]))
        .filter_map(|w| w.parse::<u32>().ok().and_then(known))
        .max();
    if four.is_some() {
        return four;
    }
    // Two or three digits before C and the latency: 36C16 (3600),
    // 600C36 (6000).
    digits
        .iter()
        .filter(|(end, run)| {
            (run.len() == 2 || run.len() == 3) && p.as_bytes().get(*end) == Some(&b'C')
        })
        .filter_map(|(_, run)| {
            let n: u32 = run.parse().ok()?;
            known(if run.len() == 2 { n * 100 } else { n * 10 })
        })
        .max()
}

/// Runs of digits, each with the index just past it.
fn runs(s: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        match (c.is_ascii_digit(), start) {
            (true, None) => start = Some(i),
            (false, Some(st)) => {
                out.push((i, &s[st..i]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(st) = start {
        out.push((s.len(), &s[st..]));
    }
    out
}

/// The memory channel a module sits on, from the names the firmware gives
/// its slot: "P0 CHANNEL A", "ChannelB-DIMM0", "DIMM_A2".
pub fn channel(bank: &str, locator: &str) -> Option<char> {
    for name in [bank, locator] {
        let up = name.to_uppercase();
        if let Some(at) = up.find("CHANNEL") {
            let letter = up[at + 7..]
                .trim_start_matches([' ', '_', '-'])
                .chars()
                .next()?;
            if letter.is_ascii_alphabetic() {
                return Some(letter);
            }
        }
        if let Some(rest) = up.split("DIMM").nth(1) {
            let rest = rest.trim_start_matches(['_', ' ', '-']);
            let mut chars = rest.chars();
            if let (Some(l), Some(d)) = (chars.next(), chars.next())
                && l.is_ascii_alphabetic()
                && d.is_ascii_digit()
            {
                return Some(l);
            }
        }
    }
    None
}

fn memory(m: &MemoryInfo) -> Vec<Check> {
    let modules: Vec<_> = m.modules.iter().filter(|x| x.size > 0).collect();
    if modules.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let running = modules
        .iter()
        .map(|x| x.configured_speed)
        .filter(|&s| s > 0)
        .min();
    let standard = modules.iter().map(|x| x.speed).max().unwrap_or(0);
    let made_for = modules
        .iter()
        .map(|x| rated_speed(&x.part_number).unwrap_or(0).max(x.speed))
        .max()
        .unwrap_or(0);
    let ddr5_four = modules.len() >= 4 && modules.iter().all(|x| x.kind == "DDR5");
    if let Some(running) = running
        && made_for > 0
    {
        let part = &modules[0].part_number;
        if running + 100 < made_for {
            let mut why = "Memory sold above its standard speed runs at the standard one until \
its profile (EXPO on AMD, XMP on Intel) is turned on in the firmware. Games held back by the \
processor run noticeably slower without it."
                .to_string();
            if ddr5_four {
                why.push_str(
                    " With four DDR5 modules many boards cannot hold the rated speed: if the PC \
is unstable at it, a step lower still beats the standard speed.",
                );
            }
            out.push(
                Check::new(
                    "memory-speed",
                    Area::Memory,
                    "Memory speed",
                    Verdict::Change,
                    format!("{running} MT/s; the modules are made for {made_for} ({part})"),
                    why,
                )
                .fix(Fix::Firmware(
                    "Turn on EXPO (AMD) or XMP (Intel) in the memory settings.".into(),
                )),
            );
        } else {
            let profile = if running > standard + 100 {
                " (its EXPO or XMP profile is on)"
            } else {
                ""
            };
            out.push(Check::new(
                "memory-speed",
                Area::Memory,
                "Memory speed",
                Verdict::Good,
                format!("{running} MT/s, as rated{profile}"),
                "",
            ));
        }
    }

    let channels: Vec<Option<char>> = modules
        .iter()
        .map(|x| channel(&x.bank, &x.locator))
        .collect();
    if modules.len() == 1 {
        out.push(Check::new(
            "memory-channels",
            Area::Memory,
            "Memory channels",
            Verdict::Consider,
            "One module: one channel",
            "The processor reads memory over two channels at once. A second module of the same \
kind doubles the bandwidth; games and integrated graphics feel it most.",
        ));
    } else if channels.iter().all(Option::is_some) {
        let mut letters: Vec<char> = channels.iter().flatten().copied().collect();
        letters.sort();
        letters.dedup();
        if letters.len() == 1 {
            out.push(
                Check::new(
                    "memory-channels",
                    Area::Memory,
                    "Memory channels",
                    Verdict::Change,
                    format!("All {} modules on channel {}", modules.len(), letters[0]),
                    "The processor reads memory over two channels at once; with every module on \
one, it has half the bandwidth.",
                )
                .fix(Fix::Hand(
                    "Move a module to the other channel's slots. The board's manual names the \
slots for two modules (usually A2 and B2)."
                        .into(),
                )),
            );
        } else {
            out.push(Check::new(
                "memory-channels",
                Area::Memory,
                "Memory channels",
                Verdict::Good,
                format!(
                    "{} modules on {} channels",
                    modules.len(),
                    letters.len()
                ),
                "",
            ));
        }
    }

    let mut kinds: Vec<&str> = modules
        .iter()
        .map(|x| x.part_number.trim())
        .filter(|p| !p.is_empty())
        .collect();
    kinds.sort();
    kinds.dedup();
    let mut sizes: Vec<u64> = modules.iter().map(|x| x.size).collect();
    sizes.sort();
    sizes.dedup();
    if kinds.len() > 1 || sizes.len() > 1 {
        out.push(Check::new(
            "memory-mixed",
            Area::Memory,
            "Mixed modules",
            Verdict::Consider,
            format!("{} kinds of module", kinds.len().max(sizes.len())),
            "Modules of different kinds run at the slowest one's speed and timings, and EXPO or \
XMP is often unstable with them. Modules from one kit work best together.",
        ));
    }
    out
}

fn page_file(files: &[PageFile]) -> Option<Check> {
    if !files.is_empty() {
        return None;
    }
    Some(if cfg!(windows) {
        Check::new(
            "page-file",
            Area::Memory,
            "Page file",
            Verdict::Consider,
            "None",
            "Windows backs the memory programs reserve with RAM and the page file. Without \
one, the limit is the RAM alone: programs fail or crash once it is promised away, even while \
memory shows free, and Windows cannot write a crash dump.",
        )
        .fix(Fix::Hand(
            "System Properties > Advanced > Performance Settings > Advanced > Virtual memory: \
let Windows manage the size."
                .into(),
        ))
    } else {
        Check::new(
            "swap",
            Area::Memory,
            "Swap",
            Verdict::Consider,
            "None",
            "With no swap, a spike in memory use ends with the kernel killing a program; a small \
swap file or zram takes the spike.",
        )
    })
}

fn displays(list: &[Display]) -> Vec<Check> {
    let fix = || {
        if cfg!(windows) {
            Fix::open("Display settings", "ms-settings:display-advanced")
        } else {
            Fix::Hand("The desktop's display settings".into())
        }
    };
    let mut out = Vec::new();
    for (i, d) in list.iter().enumerate() {
        let name = if d.name.is_empty() {
            d.device.clone()
        } else {
            d.name.clone()
        };
        if d.below_max() {
            out.push(
                Check::new(
                    format!("display-refresh-{i}"),
                    Area::Displays,
                    format!("{name}: refresh rate"),
                    Verdict::Change,
                    format!(
                        "{:.0} Hz; it does {} Hz at this resolution",
                        d.refresh_hz, d.max_hz_here
                    ),
                    "Motion looks less smooth, and what you do reaches the screen later than it \
could. Windows starts new monitors at 60 Hz.",
                )
                .fix(fix()),
            );
        }
        if d.below_native() {
            out.push(
                Check::new(
                    format!("display-resolution-{i}"),
                    Area::Displays,
                    format!("{name}: resolution"),
                    Verdict::Consider,
                    format!(
                        "{}×{}; the panel's own is {}×{}",
                        d.width, d.height, d.native.0, d.native.1
                    ),
                    "The picture is scaled up to fit the panel, so text and edges come out soft. \
Scaling (in the same settings) makes things larger without that; a game can still run at a \
lower resolution of its own.",
                )
                .fix(fix()),
            );
        }
        if !d.below_max() && !d.below_native() {
            out.push(Check::new(
                format!("display-{i}"),
                Area::Displays,
                name,
                Verdict::Good,
                format!("{}×{} at {:.0} Hz, its best", d.width, d.height, d.refresh_hz),
                "",
            ));
        }
    }
    out
}

/// A PCIe link: lanes and generation, now and at most.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Link {
    pub width: u32,
    pub max_width: u32,
    pub generation: u32,
    pub max_generation: u32,
}

pub fn generation_name(g: u32) -> String {
    match g {
        1 => "PCIe 1.1".into(),
        0 => "PCIe".into(),
        g => format!("PCIe {g}.0"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    Graphics,
    Drive,
}

/// A graphics card's or a drive's link against what it can do.
pub(crate) fn link(slot: Slot, index: usize, name: &str, l: Link) -> Option<Check> {
    if l.width == 0 || l.max_width == 0 {
        return None;
    }
    let (key, area) = match slot {
        Slot::Graphics => (format!("pcie-gpu-{index}"), Area::Graphics),
        Slot::Drive => (format!("pcie-drive-{index}"), Area::Storage),
    };
    let title = format!("{name}: PCIe link");
    Some(match slot {
        Slot::Graphics if l.width < l.max_width => {
            let fast = l.max_generation >= 4 && l.width >= 8;
            Check::new(
                key,
                area,
                title,
                if fast {
                    Verdict::Consider
                } else {
                    Verdict::Change
                },
                format!(
                    "x{} of its x{} lanes ({} card)",
                    l.width,
                    l.max_width,
                    generation_name(l.max_generation)
                ),
                "With fewer lanes the card has less bandwidth to the processor: in a slot wired \
for fewer, or sharing them with an M.2 drive. At PCIe 4.0 and above most games lose a few \
percent at x8; more when the card runs short of its own memory.",
            )
            .fix(Fix::Hand(
                "Put the card in the top long slot (the board's manual names the x16 slot from \
the processor). On some boards an M.2 drive in a given socket takes half of its lanes."
                    .into(),
            ))
        }
        Slot::Graphics => Check::new(
            key,
            area,
            title,
            Verdict::Good,
            format!(
                "x{}, all its lanes ({} card)",
                l.width,
                generation_name(l.max_generation)
            ),
            "Its link slows to an older generation while the card is idle, to save power; \
that is normal.",
        ),
        Slot::Drive if l.width < l.max_width => Check::new(
            key,
            area,
            title,
            Verdict::Change,
            format!("x{} of its x{} lanes", l.width, l.max_width),
            "Its top speed is about halved: a socket or adapter wired for fewer lanes, or lanes \
shared with another slot.",
        )
        .fix(Fix::Hand(
            "Move it to an M.2 socket wired for four lanes (the board's manual lists them).".into(),
        )),
        Slot::Drive if l.generation > 0 && l.generation < l.max_generation => Check::new(
            key,
            area,
            title,
            Verdict::Consider,
            format!(
                "{}; the drive does {}",
                generation_name(l.generation),
                generation_name(l.max_generation)
            ),
            "Each generation doubles the link's bandwidth: large copies and loading top out \
lower. An M.2 socket from the chipset, or an older board.",
        )
        .fix(Fix::Hand(
            "Move it to an M.2 socket of the drive's generation, if the board has one.".into(),
        )),
        Slot::Drive => Check::new(
            key,
            area,
            title,
            Verdict::Good,
            format!(
                "x{} at {}, all it can",
                l.width,
                generation_name(l.generation.max(l.max_generation))
            ),
            "",
        ),
    })
}

/// Integrated graphics: the processor's own, whose link and memory window
/// say nothing about a slot.
pub(crate) fn integrated(name: &str) -> bool {
    let n = name
        .to_lowercase()
        .replace("(tm)", "")
        .replace("(r)", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    n == "amd radeon graphics"
        || n.contains("uhd graphics")
        || n.contains("hd graphics")
        || n.contains("iris")
        || n == "intel arc graphics"
        || n.contains("arc 130v")
        || n.contains("arc 140v")
        || n.contains("arc 140t")
        || n.split_whitespace().any(|w| {
            // Radeon 780M, 890M, 8060S: the APUs' graphics.
            let w = w.trim_end_matches(',');
            (w.ends_with('m') || w.ends_with('s'))
                && w.len() == 4
                && w[..3].chars().all(|c| c.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hwinfo::MemoryModule;

    #[test]
    fn reads_the_speed_in_part_numbers() {
        assert_eq!(rated_speed("CMT64GX5M2B6000Z30"), Some(6000));
        assert_eq!(rated_speed("CMK16GX4M2B3200C16"), Some(3200));
        assert_eq!(rated_speed("F5-6000J3038F16G"), Some(6000));
        assert_eq!(rated_speed("F4-3600C16-8GTZNC"), Some(3600));
        assert_eq!(rated_speed("KF560C36BBE-16"), Some(6000));
        assert_eq!(rated_speed("KF436C17BB/8"), Some(3600));
        assert_eq!(rated_speed("BL16G36C16U4B.M8FE1"), Some(3600));
        assert_eq!(rated_speed("CP2K16G56C46U5"), Some(5600));
        assert_eq!(rated_speed("PVV532G600C36K"), Some(6000));
        assert_eq!(rated_speed("FF3D532G6000HC38A"), Some(6000));
        assert_eq!(rated_speed("AX5U6000C3016G-DCLARBK"), Some(6000));
        // The makers' own modules carry no speed above the standard one.
        assert_eq!(rated_speed("M323R4GA3BB0-CQKOD"), None);
        assert_eq!(rated_speed("HMCG78AGBUA081N"), None);
        assert_eq!(rated_speed(""), None);
    }

    #[test]
    fn finds_the_channel() {
        assert_eq!(channel("P0 CHANNEL A", "DIMM 0"), Some('A'));
        assert_eq!(channel("BANK 0", "Controller0-ChannelB-DIMM0"), Some('B'));
        assert_eq!(channel("", "DIMM_A2"), Some('A'));
        assert_eq!(channel("BANK 2", "DIMM 1"), None);
    }

    fn module(bank: &str, part: &str, speed: u32, running: u32) -> MemoryModule {
        MemoryModule {
            bank: bank.into(),
            locator: "DIMM 1".into(),
            size: 16 << 30,
            kind: "DDR5",
            speed,
            configured_speed: running,
            part_number: part.into(),
            ..MemoryModule::default()
        }
    }

    #[test]
    fn says_when_memory_runs_below_its_rating() {
        let m = MemoryInfo {
            modules: vec![
                module("P0 CHANNEL A", "F5-6000J3038F16G", 4800, 4800),
                module("P0 CHANNEL A", "F5-6000J3038F16G", 4800, 4800),
            ],
            ..MemoryInfo::default()
        };
        let checks = memory(&m);
        let speed = checks.iter().find(|c| c.key == "memory-speed").unwrap();
        assert_eq!(speed.verdict, Verdict::Change);
        assert!(speed.now.contains("made for 6000"), "{}", speed.now);
        let channels = checks.iter().find(|c| c.key == "memory-channels").unwrap();
        assert_eq!(channels.verdict, Verdict::Change);

        let m = MemoryInfo {
            modules: vec![
                module("P0 CHANNEL A", "CMT64GX5M2B6000Z30", 4800, 6000),
                module("P0 CHANNEL B", "CMT64GX5M2B6000Z30", 4800, 6000),
            ],
            ..MemoryInfo::default()
        };
        let checks = memory(&m);
        assert!(checks.iter().all(|c| c.verdict == Verdict::Good), "{checks:?}");
        assert!(checks[0].now.contains("profile is on"));
    }

    #[test]
    fn tells_integrated_graphics() {
        assert!(integrated("AMD Radeon(TM) Graphics"));
        assert!(integrated("AMD Radeon 780M Graphics"));
        assert!(integrated("Intel(R) UHD Graphics 770"));
        assert!(integrated("Intel(R) Arc(TM) Graphics"));
        assert!(!integrated("Intel(R) Arc(TM) A770 Graphics"));
        assert!(!integrated("NVIDIA GeForce RTX 5080"));
        assert!(!integrated("AMD Radeon RX 7900 XTX"));
    }

    #[test]
    fn judges_links() {
        let narrow = Link {
            width: 8,
            max_width: 16,
            generation: 1,
            max_generation: 5,
        };
        let c = link(Slot::Graphics, 0, "RTX", narrow).unwrap();
        assert_eq!(c.verdict, Verdict::Consider);
        let c = link(
            Slot::Drive,
            0,
            "SSD",
            Link {
                width: 4,
                max_width: 4,
                generation: 3,
                max_generation: 4,
            },
        )
        .unwrap();
        assert_eq!(c.verdict, Verdict::Consider);
        assert!(link(Slot::Drive, 0, "SSD", Link::default()).is_none());
    }
}
