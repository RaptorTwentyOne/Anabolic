//! Frame times of the programs that draw (games), and the stutters in them.

use std::sync::Arc;
use std::time::SystemTime;

#[cfg_attr(windows, path = "windows/frames.rs")]
#[cfg_attr(target_os = "linux", path = "linux/frames.rs")]
mod imp;
pub use imp::*;

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
