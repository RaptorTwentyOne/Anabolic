//! A process' open files.

use std::collections::HashMap;

use crate::model::ProcKey;

use super::*;

pub fn object_types() -> HashMap<u16, ObjectTypeInfo> {
    HashMap::new()
}

pub fn zombie_report() -> Result<ZombieReport, String> {
    Err("not available on Linux yet".into())
}

/// Every process, zombies included.
pub fn process_objects() -> u32 {
    crate::procfs::pids().len() as u32
}

pub fn handles_now(_key: ProcKey, _names: bool) -> Result<HandleReport, String> {
    Err("not available on Linux yet".into())
}

pub(super) struct Bench;

impl Bench {
    pub(super) fn new() -> Self {
        Self
    }

    pub(super) fn handles(&mut self, key: ProcKey, names: bool) -> Result<HandleReport, String> {
        handles_now(key, names)
    }

    pub(super) fn modules(
        &mut self,
        key: ProcKey,
        _addresses: &[u64],
        _tids: &[u32],
    ) -> ModuleReport {
        let started = std::time::Instant::now();
        let (modules, error) = match crate::modules::process_modules(key.pid) {
            Ok(m) => (m, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        ModuleReport {
            key,
            modules: modules
                .into_iter()
                .map(|info| ModuleRow {
                    info,
                    description: None,
                    company: None,
                    version: None,
                })
                .collect(),
            error,
            symbols: HashMap::new(),
            thread_names: HashMap::new(),
            took: started.elapsed(),
        }
    }

    pub(super) fn stack(
        &mut self,
        key: ProcKey,
        tid: u32,
    ) -> Result<crate::threads::StackReport, String> {
        crate::threads::call_stack(key, tid, &mut crate::modules::Symbolizer::default())
    }

    pub(super) fn find(&mut self, _query: &str, _exclude: &[String]) -> Result<FindReport, String> {
        Err("not available on Linux yet".into())
    }
}
