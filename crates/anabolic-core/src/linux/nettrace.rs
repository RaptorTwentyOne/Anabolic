//! Network traffic per process and per connection.

use super::*;

pub struct NetTracer {
    _private: (),
}

impl NetTracer {
    pub fn start() -> Result<Self, String> {
        Err("not available on Linux yet".into())
    }

    pub fn take(&self) -> Traffic {
        Traffic::default()
    }
}
