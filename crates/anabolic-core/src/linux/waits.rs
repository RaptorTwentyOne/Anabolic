//! Why a program does not respond.

use crate::model::ProcKey;

use super::*;

/// A Wayland compositor tells no one which windows stopped answering.
pub fn hung_windows() -> Vec<HungWindow> {
    Vec::new()
}

pub fn wait_chains(
    _key: ProcKey,
    _first: Option<u32>,
    _keep_going: &mut dyn FnMut(usize, usize) -> bool,
) -> Result<WaitReport, String> {
    Err("not available on Linux yet".into())
}
