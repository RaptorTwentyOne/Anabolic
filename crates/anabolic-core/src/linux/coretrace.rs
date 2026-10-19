//! Who runs on each logical processor.

use super::*;

pub struct CoreTracer {
    _private: (),
}

impl CoreTracer {
    pub fn start(_cpus: usize) -> Result<Self, String> {
        Err("not available on Linux yet".into())
    }

    pub fn take(&self) -> CoreSamples {
        CoreSamples::default()
    }
}
