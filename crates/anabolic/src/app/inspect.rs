// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Work started on a selected process off the UI thread: its handles, wait
//! chains, modules and a thread's stack, and the side pane that shows them.

use super::*;

impl App {
    pub(super) fn inspect_handles(&mut self, key: ProcKey, names: bool) {
        if self.table.selected != Some(key) {
            self.select(Some(key));
            self.table.scroll_to_selected = true;
        }
        self.detail_tab = DetailTab::Handles;
        self.handle_view.pending = Some(key);
        self.handle_view.error = None;
        self.inspector.request(InspectRequest::Handles {
            key,
            resolve_names: names,
        });
    }

    /// Follows what the process' threads wait for, a hung window's thread
    /// first, and shows the answer in its details.
    pub(super) fn analyze_waits(&mut self, key: ProcKey) {
        let first = self
            .snapshot
            .system
            .hung
            .iter()
            .find(|h| h.pid == key.pid)
            .map(|h| h.tid);
        self.analyze_waits_from(key, first);
    }

    /// Like `analyze_waits`, with `first` analysed and listed first.
    pub(super) fn analyze_waits_from(&mut self, key: ProcKey, first: Option<u32>) {
        self.wait_view.pending = Some(key);
        self.wait_view.started = Some(std::time::Instant::now());
        self.wait_view.progress = None;
        self.wait_view.error = None;
        self.inspector
            .request(InspectRequest::WaitChain { key, first });
        self.go_to(key);
        self.detail_tab = DetailTab::Waits;
        self.settings.show_details = true;
    }

    /// `--pane`: waits for what the tab needs, then selects the row.
    pub(super) fn open_pending_pane(&mut self) {
        let Some(key) = self.table.selected else {
            return;
        };
        let name = self.pending_pane.clone().unwrap_or_default().to_lowercase();
        match self.detail_tab {
            DetailTab::Threads => {
                let Some(first) = self
                    .snapshot
                    .selected
                    .as_ref()
                    .filter(|s| s.key == key)
                    .and_then(|s| s.threads.iter().min_by_key(|t| t.create_time))
                    .map(|t| t.tid)
                else {
                    return;
                };
                self.focus_thread = Some((first, false));
                self.load_modules(key);
                if self.pending_stack {
                    self.panes.stack_ok.insert(key);
                    self.read_stack(key, first);
                }
            }
            DetailTab::Modules => {
                let Some(report) = self.module_view.report.clone().filter(|r| r.key == key) else {
                    self.load_modules(key);
                    return;
                };
                self.module_view.selected = report
                    .modules
                    .iter()
                    .find(|m| m.info.name.to_lowercase() == name)
                    .or(report.modules.get(1))
                    .map(|m| m.info.base);
            }
            DetailTab::Handles => {
                let Some(report) = self.handle_view.report.clone().filter(|r| r.key == key) else {
                    return;
                };
                self.handle_view.selected = report
                    .entries
                    .iter()
                    .find(|e| report.type_name(e.type_index).to_lowercase() == name)
                    .or(report.entries.first())
                    .map(|e| e.value);
            }
            DetailTab::Network => {
                // The busiest connection, once some traffic was seen.
                let Some(c) = self
                    .snapshot
                    .selected
                    .as_ref()
                    .filter(|s| s.key == key)
                    .and_then(|s| s.connections.iter().max_by_key(|c| c.rx_total + c.tx_total))
                else {
                    return;
                };
                self.tables.connection = Some((c.proto, c.remote, c.remote_port, c.local_port));
            }
            _ => {}
        }
        self.pending_pane = None;
    }

    pub(super) fn read_stack(&mut self, key: ProcKey, tid: u32) {
        self.panes.stack_pending = Some((key, tid));
        self.panes.stack_error = None;
        self.inspector.request(InspectRequest::Stack { key, tid });
    }

    pub(super) fn load_modules(&mut self, key: ProcKey) {
        if self.module_view.pending == Some(key) {
            return;
        }
        let addresses = self
            .snapshot
            .selected
            .as_ref()
            .filter(|s| s.key == key)
            .map(|s| {
                s.threads
                    .iter()
                    .map(|t| {
                        if t.win32_start_address != 0 {
                            t.win32_start_address
                        } else {
                            t.start_address
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let tids = self
            .snapshot
            .selected
            .as_ref()
            .filter(|s| s.key == key)
            .map(|s| s.threads.iter().map(|t| t.tid).collect())
            .unwrap_or_default();
        self.module_view.pending = Some(key);
        self.inspector.request(InspectRequest::Modules {
            key,
            addresses,
            tids,
        });
    }
}
