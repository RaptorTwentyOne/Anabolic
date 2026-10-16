//! Frame times of the programs that draw: every present a process makes,
//! from the graphics runtimes' own ETW events (PresentMon's method). Nothing
//! is opened or injected: Windows asks the runtime inside each process to log
//! its presents to a trace session of ours, which is what PresentMon,
//! CapFrameX and the GPU vendors' overlays-free tools rely on beside
//! anti-cheat protected games.
//!
//! Sources, by preference: DXGI (Direct3D 10 to 12, and Vulkan or OpenGL
//! presented through a DXGI swap chain), Direct3D 9, and for programs that use
//! neither (Vulkan or OpenGL presenting on their own) the kernel's present
//! event from DxgKrnl. The kernel one is chatty as a provider (some 16,000
//! events a second on a busy desktop, all but one kind dropped by an event id
//! filter before they are buffered), so it can be left out.
//!
//! The times are the intervals between present calls, the CPU side of a
//! frame, which is what frame-time graphs usually show; when a frame reached
//! the screen is not followed.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW,
    MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible,
};
use windows::core::{BOOL, GUID, PCWSTR};

use crate::etw::{self, QpcClock, Session};

const DXGI: GUID = GUID::from_u128(0xCA11C036_0102_4A2D_A6AD_F03CFED5D3C9);
const D3D9: GUID = GUID::from_u128(0x783ACA0A_790E_4D7F_8451_AA850511C6B9);
const DXGKRNL: GUID = GUID::from_u128(0x802EC45A_1E99_4B83_9920_87C98277BA9D);
const DXGI_PRESENT_START: u16 = 42;
const DXGI_PRESENT_MPO_START: u16 = 55;
const D3D9_PRESENT_START: u16 = 1;
/// DxgKrnl's present (Present_Info): one per present that reaches the
/// kernel, whatever the API. Measured one to one with DXGI's Present_Start.
const DXGKRNL_PRESENT: u16 = 184;
/// DxgKrnl's Base keyword carries the present event.
const DXGKRNL_BASE: u64 = 0x1;
const LEVEL_VERBOSE: u8 = 5;
/// A present that only asks whether the window is visible (minimized games
/// keep making them); nothing is drawn.
const DXGI_PRESENT_TEST: u32 = 0x1;

/// Frames kept per swap chain; a few seconds even at several hundred fps.
const MAX_FRAMES: usize = 4096;
/// Statistics cover this much of the recent past.
const WINDOW_MS: f64 = 10_000.0;
/// Frame times handed to the UI per process, for a graph.
const GRAPH_FRAMES: usize = 1000;
/// Stutters kept, all processes together.
const MAX_STUTTERS: usize = 256;
/// A program is listed while it presents at least this many frames in the
/// last two seconds (5 fps): a browser that repaints twice a minute is not
/// drawing frames.
const REGULAR_FRAMES: usize = 10;
/// The kernel's present events of a program are ignored while its runtime
/// (DXGI, D3D9) reported a present this recently: the same frames, twice.
const RUNTIME_PREFERRED_MS: f64 = 2000.0;

/// Stutter judgement: the frames before a frame say what is typical, a few
/// after it say whether the rate dropped for good (then it is a slower pace,
/// not a hitch).
const BEFORE: usize = 30;
const AFTER: usize = 4;
/// Too slow to be a game playing (under 20 fps): no stutters there.
const REGULAR_MS: f32 = 50.0;
/// Longer than this is a pause (alt-tab, a loading screen), not a hitch.
const PAUSE_MS: f32 = 2000.0;
/// A pace is only broken once it was kept up: over the two seconds before a
/// frame, at least 70 % of the frames the typical time makes room for. A
/// window that repaints in bursts (while the mouse moves) pauses between
/// them; a game does not.
const SUSTAIN_MS: f32 = 2000.0;
const SUSTAINED: f32 = 0.7;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresentApi {
    Dxgi,
    D3d9,
    /// Seen only by the kernel: Vulkan, OpenGL or anything else presenting
    /// without DXGI or Direct3D 9.
    Other,
}

impl PresentApi {
    pub fn label(self) -> &'static str {
        match self {
            PresentApi::Dxgi => "DXGI",
            PresentApi::D3d9 => "Direct3D 9",
            PresentApi::Other => "Vulkan / OpenGL",
        }
    }
}

/// The window a program draws in, and the monitor it is on.
#[derive(Clone, Debug)]
pub struct PresentWindow {
    /// The display device, like `\\.\DISPLAY2`.
    pub monitor: Arc<str>,
    pub primary: bool,
    /// The monitor's current refresh rate (its maximum under variable
    /// refresh).
    pub refresh_hz: u32,
    pub width: u32,
    pub height: u32,
    /// Covers its whole monitor: fullscreen or borderless.
    pub fullscreen: bool,
    pub foreground: bool,
}

#[derive(Clone, Debug)]
pub struct ProcessFrames {
    pub pid: u32,
    /// Filled in by the collector from its process list.
    pub name: Arc<str>,
    pub api: PresentApi,
    /// Presents in the last second.
    pub fps: f32,
    /// Over the last ten seconds.
    pub avg_ms: f32,
    /// 99th percentile frame time over the last ten seconds.
    pub p99_ms: f32,
    /// "1 % low": the frame rate the slowest 1 % of frames ran at
    /// (1000 / p99_ms).
    pub low_fps: f32,
    pub max_ms: f32,
    /// Stutters in the last ten seconds.
    pub stutters: u32,
    /// The most recent frame times in ms, oldest first.
    pub frame_times: Arc<[f32]>,
    pub window: Option<PresentWindow>,
}

/// A frame that took far longer than the ones around it.
#[derive(Clone, Debug)]
pub struct Stutter {
    pub pid: u32,
    pub name: Arc<str>,
    /// When the late frame was presented.
    pub at: SystemTime,
    pub frame_ms: f32,
    /// The typical frame time just before it.
    pub typical_ms: f32,
}

#[derive(Clone, Debug, Default)]
pub struct FrameStats {
    /// Programs drawing frames now, the highest frame rate first.
    pub processes: Vec<ProcessFrames>,
    /// Since tracing started, newest last.
    pub stutters: Vec<Stutter>,
    /// Vulkan and OpenGL programs are seen too (DxgKrnl is on).
    pub kernel_presents: bool,
    /// Events the session dropped so far.
    pub lost_events: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Source {
    Dxgi,
    D3d9,
    Kernel,
}

/// One swap chain's presents (one per process for the kernel source).
struct Chain {
    pid: u32,
    source: Source,
    /// (timestamp of the present, ms since the previous one), oldest first.
    frames: VecDeque<(i64, f32)>,
    last: i64,
}

struct RawStutter {
    pid: u32,
    at: i64,
    frame_ms: f32,
    typical_ms: f32,
}

#[derive(Default)]
struct Shared {
    chains: HashMap<(u32, u64, Source), Chain>,
    /// pid -> latest runtime present.
    runtime_seen: HashMap<u32, i64>,
    stutters: VecDeque<RawStutter>,
    /// The newest timestamp delivered: events arrive up to a second late
    /// (the session flushes once a second), so rates are measured up to here.
    latest: i64,
}

impl Shared {
    /// A present `pid` made at `at`, from `source` on swap chain `chain`.
    /// `preferred`: how recent a runtime present makes the kernel's view of
    /// the same process redundant.
    fn present(
        &mut self,
        pid: u32,
        source: Source,
        chain: u64,
        at: i64,
        clock: &QpcClock,
        preferred: i64,
    ) {
        self.latest = self.latest.max(at);
        if source == Source::Kernel {
            if self
                .runtime_seen
                .get(&pid)
                .is_some_and(|&seen| at - seen < preferred)
            {
                return;
            }
        } else {
            self.runtime_seen.insert(pid, at);
        }
        let chain = self
            .chains
            .entry((pid, chain, source))
            .or_insert_with(|| Chain {
                pid,
                source,
                frames: VecDeque::new(),
                last: at,
            });
        if at <= chain.last {
            // The first present of a chain only starts the clock.
            chain.last = chain.last.max(at);
            return;
        }
        if let Some(stutter) = chain.push(at, clock) {
            if self.stutters.len() == MAX_STUTTERS {
                self.stutters.pop_front();
            }
            self.stutters.push_back(stutter);
        }
    }
}

/// What a present event says: its source and swap chain. None for events
/// that are not presents, or presents that draw nothing.
fn parse_present(provider: &GUID, id: u16, data: &[u8], pointer: usize) -> Option<(Source, u64)> {
    if *provider == DXGI && (id == DXGI_PRESENT_START || id == DXGI_PRESENT_MPO_START) {
        // pIDXGISwapChain, Flags, SyncInterval.
        let chain = etw::pointer_at(data, 0, pointer)?;
        let flags = etw::u32_at(data, pointer)?;
        (flags & DXGI_PRESENT_TEST == 0).then_some((Source::Dxgi, chain))
    } else if *provider == D3D9 && id == D3D9_PRESENT_START {
        // pSwapchain, Flags.
        Some((Source::D3d9, etw::pointer_at(data, 0, pointer)?))
    } else if *provider == DXGKRNL && id == DXGKRNL_PRESENT {
        Some((Source::Kernel, 0))
    } else {
        None
    }
}

/// The middle value; reorders `v`.
fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mid = v.len() / 2;
    *v.select_nth_unstable_by(mid, f32::total_cmp).1
}

/// Whether a frame that took `ms` is a stutter, given the frames just before
/// and just after it; the typical frame time if so. Only a steady pace can
/// be broken: a program that draws when something changes (a terminal, a
/// browser) has no typical frame. A stutter takes twice the typical time and
/// at least 8 ms more (at 144 fps a 15 ms frame, at 60 fps one longer than
/// two refreshes), and the frames after it are back to normal: a lasting
/// drop in frame rate is a slower pace, not a hitch.
fn judge(ms: f32, before: &mut [f32], after: &mut [f32]) -> Option<f32> {
    let typical = median(before);
    // Four in five frames within half again the typical time.
    let steady = before.len() >= 5 && {
        let at = before.len() * 4 / 5;
        *before.select_nth_unstable_by(at, f32::total_cmp).1 <= typical * 1.5
    };
    let next = median(after);
    let stutter = steady
        && typical > 0.0
        && typical < REGULAR_MS
        && ms <= PAUSE_MS
        && ms > typical * 2.0
        && ms > typical + 8.0
        && ms > next * 1.5;
    stutter.then_some(typical)
}

/// The value below which `p` (0..1) of the sorted values fall, nearest rank.
fn percentile(sorted: &[f32], p: f64) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

impl Chain {
    /// Adds a present and judges the frame `AFTER` presents back, now that
    /// the frames after it are known.
    fn push(&mut self, at: i64, clock: &QpcClock) -> Option<RawStutter> {
        let ms = clock.ms(at - self.last);
        self.last = at;
        if self.frames.len() == MAX_FRAMES {
            self.frames.pop_front();
        }
        self.frames.push_back((at, ms));
        let n = self.frames.len();
        if n < BEFORE + AFTER + 1 {
            return None;
        }
        let at_judged = n - 1 - AFTER;
        let mut before = [0f32; BEFORE];
        let mut after = [0f32; AFTER];
        for (slot, i) in before.iter_mut().zip(at_judged - BEFORE..at_judged) {
            *slot = self.frames[i].1;
        }
        for (slot, i) in after.iter_mut().zip(at_judged + 1..n) {
            *slot = self.frames[i].1;
        }
        let (when, ms) = self.frames[at_judged];
        let typical_ms = judge(ms, &mut before, &mut after)?;
        let start = self.frames[at_judged - 1].0;
        let span = clock.from_ms(SUSTAIN_MS as f64);
        let first = self.frames.partition_point(|&(t, _)| t <= start - span);
        let kept_up = (at_judged - first) as f32 >= SUSTAINED * SUSTAIN_MS / typical_ms;
        kept_up.then_some(RawStutter {
            pid: self.pid,
            at: when,
            frame_ms: ms,
            typical_ms,
        })
    }
}

/// Frame rate, average, 99th percentile and longest frame over `frames`
/// (all inside the window), with `latest` as the end of the last second.
struct Summary {
    fps: f32,
    avg_ms: f32,
    p99_ms: f32,
    max_ms: f32,
}

fn summarize(frames: &VecDeque<(i64, f32)>, latest: i64, second: i64) -> Summary {
    let fps = frames
        .iter()
        .filter(|(at, _)| *at > latest - second)
        .count() as f32;
    let mut times: Vec<f32> = frames.iter().map(|&(_, ms)| ms).collect();
    times.sort_unstable_by(f32::total_cmp);
    let avg_ms = if times.is_empty() {
        0.0
    } else {
        times.iter().sum::<f32>() / times.len() as f32
    };
    Summary {
        fps,
        avg_ms,
        p99_ms: percentile(&times, 0.99),
        max_ms: times.last().copied().unwrap_or(0.0),
    }
}

pub struct FrameTracer {
    shared: Arc<Mutex<Shared>>,
    clock: QpcClock,
    kernel: bool,
    session: Session,
}

impl FrameTracer {
    /// `kernel`: also follow programs that present without DXGI or
    /// Direct3D 9 (Vulkan, OpenGL), through DxgKrnl.
    pub fn start(kernel: bool) -> Result<Self, String> {
        let session = Session::start("Anabolic-Frames", 0, 0)?;
        session.enable_ids(
            &DXGI,
            LEVEL_VERBOSE,
            u64::MAX,
            &[DXGI_PRESENT_START, DXGI_PRESENT_MPO_START],
        )?;
        session.enable_ids(&D3D9, LEVEL_VERBOSE, u64::MAX, &[D3D9_PRESENT_START])?;
        if kernel {
            session.enable_ids(&DXGKRNL, LEVEL_VERBOSE, DXGKRNL_BASE, &[DXGKRNL_PRESENT])?;
        }
        let clock = QpcClock::new();
        let preferred = clock.from_ms(RUNTIME_PREFERRED_MS);
        let shared = Arc::new(Mutex::new(Shared::default()));
        let sink = shared.clone();
        // Anabolic repaints when data arrives: not a game, and listing its
        // own frames (and "stutters") every refresh is noise.
        let own = std::process::id();
        session.consume(move |record| {
            let header = &record.EventHeader;
            if header.ProcessId == own {
                return;
            }
            let Some((source, chain)) = parse_present(
                &header.ProviderId,
                header.EventDescriptor.Id,
                etw::user_data(record),
                etw::pointer_size(record),
            ) else {
                return;
            };
            if let Ok(mut shared) = sink.lock() {
                let at = header.TimeStamp;
                shared.present(header.ProcessId, source, chain, at, &clock, preferred);
            }
        })?;
        Ok(Self {
            shared,
            clock,
            kernel,
            session,
        })
    }

    /// The programs drawing now and the recent stutters. `name_of` names a
    /// process; the windows are looked up here (they do not touch the
    /// programs).
    pub fn take(&mut self, name_of: impl Fn(u32) -> Option<Arc<str>>) -> FrameStats {
        let now = QpcClock::ticks();
        let window = self.clock.from_ms(WINDOW_MS);
        let second = self.clock.freq;
        let Ok(mut shared) = self.shared.lock() else {
            return FrameStats::default();
        };
        let latest = shared.latest;
        // Forget what has stopped presenting.
        shared.chains.retain(|_, c| now - c.last < window);
        shared
            .runtime_seen
            .retain(|_, &mut seen| now - seen < window);
        for chain in shared.chains.values_mut() {
            while chain
                .frames
                .front()
                .is_some_and(|&(at, _)| at < latest - window)
            {
                chain.frames.pop_front();
            }
        }
        // Per process the busiest runtime chain, or the kernel's view when the
        // runtimes said nothing.
        let mut best: HashMap<u32, &Chain> = HashMap::new();
        for chain in shared.chains.values() {
            let recent = chain
                .frames
                .iter()
                .rev()
                .take_while(|(at, _)| *at > latest - 2 * second)
                .count();
            if recent < REGULAR_FRAMES {
                continue;
            }
            let rank = |c: &Chain| {
                let recent = c
                    .frames
                    .iter()
                    .rev()
                    .take_while(|(at, _)| *at > latest - 2 * second)
                    .count();
                (c.source != Source::Kernel, recent)
            };
            match best.get(&chain.pid) {
                Some(current) if rank(current) >= rank(chain) => {}
                _ => {
                    best.insert(chain.pid, chain);
                }
            }
        }
        let pids: HashSet<u32> = best.keys().copied().collect();
        let windows = if pids.is_empty() {
            HashMap::new()
        } else {
            windows_of(&pids)
        };
        let mut processes: Vec<ProcessFrames> = best
            .values()
            .map(|chain| {
                let s = summarize(&chain.frames, latest, second);
                let skip = chain.frames.len().saturating_sub(GRAPH_FRAMES);
                ProcessFrames {
                    pid: chain.pid,
                    name: name_of(chain.pid).unwrap_or_else(|| Arc::from("")),
                    api: match chain.source {
                        Source::Dxgi => PresentApi::Dxgi,
                        Source::D3d9 => PresentApi::D3d9,
                        Source::Kernel => PresentApi::Other,
                    },
                    fps: s.fps,
                    avg_ms: s.avg_ms,
                    p99_ms: s.p99_ms,
                    low_fps: if s.p99_ms > 0.0 {
                        1000.0 / s.p99_ms
                    } else {
                        0.0
                    },
                    max_ms: s.max_ms,
                    stutters: shared
                        .stutters
                        .iter()
                        .filter(|st| st.pid == chain.pid && st.at > latest - window)
                        .count() as u32,
                    frame_times: chain.frames.iter().skip(skip).map(|&(_, ms)| ms).collect(),
                    window: windows.get(&chain.pid).cloned(),
                }
            })
            .collect();
        processes.sort_by(|a, b| b.fps.total_cmp(&a.fps).then(a.pid.cmp(&b.pid)));
        let stutters = shared
            .stutters
            .iter()
            .map(|s| Stutter {
                pid: s.pid,
                name: name_of(s.pid).unwrap_or_else(|| Arc::from("")),
                at: self.clock.wall(s.at),
                frame_ms: s.frame_ms,
                typical_ms: s.typical_ms,
            })
            .collect();
        drop(shared);
        FrameStats {
            processes,
            stutters,
            kernel_presents: self.kernel,
            lost_events: self.session.lost().0,
        }
    }
}

/// The largest visible window of each process, and its monitor. Window
/// manager queries only; the programs themselves are not asked anything.
fn windows_of(pids: &HashSet<u32>) -> HashMap<u32, PresentWindow> {
    struct Search<'a> {
        pids: &'a HashSet<u32>,
        /// pid -> (window, area)
        found: HashMap<u32, (HWND, i64)>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                return BOOL(1);
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if !search.pids.contains(&pid) {
                return BOOL(1);
            }
            // On another virtual desktop, or a hidden app frame.
            let mut cloaked = 0u32;
            if DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                (&mut cloaked as *mut u32).cast(),
                size_of::<u32>() as u32,
            )
            .is_ok()
                && cloaked != 0
            {
                return BOOL(1);
            }
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() {
                return BOOL(1);
            }
            let area = (rect.right - rect.left) as i64 * (rect.bottom - rect.top) as i64;
            let best = search.found.entry(pid).or_insert((hwnd, area));
            if area > best.1 {
                *best = (hwnd, area);
            }
        }
        BOOL(1)
    }
    let mut search = Search {
        pids,
        found: HashMap::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut search as *mut Search as isize));
    }
    let foreground = unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid));
        pid
    };
    search
        .found
        .into_iter()
        .filter_map(|(pid, (hwnd, _))| {
            let mut window = monitor_of(hwnd)?;
            window.foreground = pid == foreground;
            Some((pid, window))
        })
        .collect()
}

fn monitor_of(hwnd: HWND) -> Option<PresentWindow> {
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(monitor, &mut info.monitorInfo).as_bool() {
            return None;
        }
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        let device = PCWSTR(info.szDevice.as_ptr());
        let refresh_hz = if EnumDisplaySettingsW(device, ENUM_CURRENT_SETTINGS, &mut mode).as_bool()
        {
            mode.dmDisplayFrequency
        } else {
            0
        };
        let mut rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);
        let m = info.monitorInfo.rcMonitor;
        const MONITORINFOF_PRIMARY: u32 = 1;
        Some(PresentWindow {
            monitor: Arc::from(crate::util::from_wide(&info.szDevice)),
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            refresh_hz,
            width: (m.right - m.left) as u32,
            height: (m.bottom - m.top) as u32,
            fullscreen: rect.left <= m.left
                && rect.top <= m.top
                && rect.right >= m.right
                && rect.bottom >= m.bottom,
            foreground: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presents_are_parsed_per_api() {
        let mut dxgi = vec![0u8; 24];
        dxgi[..8].copy_from_slice(&0x1234_5678_9abcu64.to_le_bytes());
        assert_eq!(
            parse_present(&DXGI, 42, &dxgi, 8),
            Some((Source::Dxgi, 0x1234_5678_9abc))
        );
        // A visibility test draws nothing.
        dxgi[8] = 1;
        assert_eq!(parse_present(&DXGI, 42, &dxgi, 8), None);
        // 32-bit process: 4-byte pointer, flags right after it.
        let mut d3d9 = vec![0u8; 8];
        d3d9[..4].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        assert_eq!(
            parse_present(&D3D9, 1, &d3d9, 4),
            Some((Source::D3d9, 0xdead_beef))
        );
        assert_eq!(
            parse_present(&DXGKRNL, 184, &[0; 44], 8),
            Some((Source::Kernel, 0))
        );
        // Present_Stop and other events are not presents.
        assert_eq!(parse_present(&DXGI, 43, &dxgi, 8), None);
        assert_eq!(parse_present(&DXGI, 42, &[0; 4], 8), None);
    }

    #[test]
    fn percentiles_by_nearest_rank() {
        let sorted: Vec<f32> = (1..=100).map(|v| v as f32).collect();
        assert_eq!(percentile(&sorted, 0.99), 99.0);
        assert_eq!(percentile(&sorted, 0.5), 50.0);
        assert_eq!(percentile(&sorted, 1.0), 100.0);
        assert_eq!(percentile(&[7.0], 0.99), 7.0);
        assert_eq!(percentile(&[], 0.99), 0.0);
        let mut v = [5.0, 1.0, 3.0];
        assert_eq!(median(&mut v), 3.0);
    }

    #[test]
    fn a_hitch_is_a_stutter_but_a_slower_pace_is_not() {
        let at_144 = || [6.9f32; BEFORE];
        // One 40 ms frame among 144 fps ones.
        assert_eq!(judge(40.0, &mut at_144(), &mut [6.9; AFTER]), Some(6.9));
        // Too small a bump to see: 12 ms at 144 fps.
        assert_eq!(judge(12.0, &mut at_144(), &mut [6.9; AFTER]), None);
        // The game drops to 30 fps and stays there.
        assert_eq!(judge(33.3, &mut at_144(), &mut [33.3; AFTER]), None);
        // 60 fps with vsync: two refreshes is pacing, more is a hitch.
        assert_eq!(judge(33.3, &mut [16.7; BEFORE], &mut [16.7; AFTER]), None);
        assert_eq!(
            judge(50.0, &mut [16.7; BEFORE], &mut [16.7; AFTER]),
            Some(16.7)
        );
        // Alt-tab or a loading screen.
        assert_eq!(judge(5000.0, &mut at_144(), &mut [6.9; AFTER]), None);
        // A program drawing at 5 fps is not a game running.
        assert_eq!(
            judge(800.0, &mut [200.0; BEFORE], &mut [200.0; AFTER]),
            None
        );
        // Nor is one that draws when something changes.
        let mut uneven: [f32; BEFORE] = std::array::from_fn(|i| [7.0, 7.0, 30.0][i % 3]);
        assert_eq!(judge(90.0, &mut uneven, &mut [7.0; AFTER]), None);
        // A steady game with a hitch or two before still counts.
        let mut bumpy = at_144();
        bumpy[3] = 40.0;
        bumpy[17] = 25.0;
        assert_eq!(judge(40.0, &mut bumpy, &mut [6.9; AFTER]), Some(6.9));
    }

    #[test]
    fn the_kernel_view_counts_only_without_a_runtime_one() {
        let clock = QpcClock::new();
        let preferred = clock.from_ms(RUNTIME_PREFERRED_MS);
        let mut shared = Shared::default();
        let frame = clock.from_ms(16.7);
        for i in 1..=20 {
            let at = i * frame;
            // A DXGI game: its runtime and the kernel both report each frame.
            shared.present(1, Source::Dxgi, 0xabc, at, &clock, preferred);
            shared.present(1, Source::Kernel, 0, at + 10, &clock, preferred);
            // A Vulkan game: only the kernel does.
            shared.present(2, Source::Kernel, 0, at, &clock, preferred);
        }
        assert_eq!(shared.chains.len(), 2);
        assert_eq!(shared.chains[&(1, 0xabc, Source::Dxgi)].frames.len(), 19);
        assert_eq!(shared.chains[&(2, 0, Source::Kernel)].frames.len(), 19);
        assert!(!shared.chains.contains_key(&(1, 0, Source::Kernel)));
    }

    #[test]
    fn chains_find_the_stutter_after_the_frames_that_follow_it() {
        let clock = QpcClock::new();
        let mut chain = Chain {
            pid: 7,
            source: Source::Dxgi,
            frames: VecDeque::new(),
            last: 0,
        };
        let mut at = 0i64;
        let mut found = Vec::new();
        // 400 frames at ~7 ms, one at 45 ms, 10 more at ~7 ms.
        let times: Vec<f64> = (0..400)
            .map(|i| 6.9 + (i % 3) as f64 * 0.1)
            .chain([45.0])
            .chain((0..10).map(|_| 7.0))
            .collect();
        for ms in &times {
            at += clock.from_ms(*ms);
            if let Some(s) = chain.push(at, &clock) {
                found.push(s);
            }
        }
        assert_eq!(found.len(), 1, "one stutter expected");
        assert!((found[0].frame_ms - 45.0).abs() < 0.01);
        assert!((found[0].typical_ms - 7.0).abs() < 0.2);
        let s = summarize(&chain.frames, at, clock.freq);
        assert!((s.max_ms - 45.0).abs() < 0.01);
        assert!(s.fps >= 135.0 && s.fps <= 145.0, "fps {}", s.fps);
        assert!(s.avg_ms > 7.0 && s.avg_ms < 7.2, "avg {}", s.avg_ms);
    }

    #[test]
    fn a_window_repainting_in_bursts_does_not_stutter() {
        let clock = QpcClock::new();
        let mut chain = Chain {
            pid: 7,
            source: Source::Dxgi,
            frames: VecDeque::new(),
            last: 0,
        };
        let mut at = 0i64;
        let mut found = 0;
        // 60 frames at 144 fps while the mouse moves, then 300 ms still.
        for _ in 0..10 {
            for i in 0..60 {
                at += clock.from_ms(if i == 0 { 300.0 } else { 6.9 });
                found += chain.push(at, &clock).is_some() as u32;
            }
        }
        assert_eq!(found, 0);
    }
}
