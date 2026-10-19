//! Who reads and writes each physical disk.

use super::*;

pub struct DiskTracer {
    _private: (),
}

impl DiskTracer {
    pub fn start() -> Result<Self, String> {
        Err("not available on Linux yet".into())
    }

    /// Events lost and buffers lost.
    pub fn lost(&self) -> (u32, u32) {
        (0, 0)
    }

    pub fn take(&self) -> DiskSample {
        DiskSample::default()
    }
}
