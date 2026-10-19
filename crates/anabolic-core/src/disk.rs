//! Physical disks: what they are, how busy, and the volumes on them.

#[cfg_attr(windows, path = "windows/disk.rs")]
#[cfg_attr(target_os = "linux", path = "linux/disk.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Debug, Default)]
pub struct Volume {
    /// "C:"
    pub letter: String,
    pub label: String,
    pub total: u64,
    pub free: u64,
}

#[derive(Clone, Debug, Default)]
pub struct DiskStats {
    pub number: u32,
    pub model: String,
    pub bus: &'static str,
    pub ssd: Option<bool>,
    pub size: u64,
    pub volumes: Vec<Volume>,
    pub read_rate: f64,
    pub write_rate: f64,
    pub read_iops: f64,
    pub write_iops: f64,
    /// Share of the interval the disk had I/O outstanding, 0..=100.
    pub active: f32,
    pub response_ms: f32,
    pub queue: u32,
    pub temperature: Option<i16>,
}
