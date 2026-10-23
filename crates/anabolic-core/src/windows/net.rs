// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! System-wide network throughput, per physical adapter.

use std::collections::HashMap;

use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;

use crate::util::from_wide;

use super::*;

impl NetSampler {
    /// Counts only hardware adapters that are up: virtual switches, VPN
    /// tunnels and loopback would count the same bytes twice.
    pub fn sample(&mut self, secs: f64) -> NetStats {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if unsafe { GetIfTable2(&mut table) }.is_err() || table.is_null() {
            return NetStats::default();
        }
        let rows = unsafe {
            let count = (*table).NumEntries as usize;
            std::slice::from_raw_parts((*table).Table.as_ptr(), count)
        };
        let mut stats = NetStats::default();
        let mut next = HashMap::with_capacity(self.prev.len());
        for row in rows {
            let hardware = row.InterfaceAndOperStatusFlags._bitfield & 1 != 0;
            if !hardware || row.OperStatus != IfOperStatusUp {
                continue;
            }
            let luid = unsafe { row.InterfaceLuid.Value };
            let (rx_total, tx_total) = (row.InOctets, row.OutOctets);
            let (rx_rate, tx_rate) = match self.prev.get(&luid) {
                Some(&(rx, tx)) => (
                    rx_total.saturating_sub(rx) as f64 / secs,
                    tx_total.saturating_sub(tx) as f64 / secs,
                ),
                None => (0.0, 0.0),
            };
            next.insert(luid, (rx_total, tx_total));
            stats.rx_rate += rx_rate;
            stats.tx_rate += tx_rate;
            stats.adapters.push(AdapterStats {
                luid,
                name: from_wide(&row.Alias),
                description: from_wide(&row.Description),
                link_speed: row.ReceiveLinkSpeed.max(row.TransmitLinkSpeed),
                rx_rate,
                tx_rate,
                rx_total,
                tx_total,
            });
        }
        unsafe { FreeMibTable(table.cast()) };
        self.prev = next;
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_an_adapter_quickly() {
        let mut sampler = NetSampler::default();
        // The quickest of three: one read can wait on a machine busy with
        // the rest of the tests, every read cannot.
        let mut quickest = std::time::Duration::MAX;
        for _ in 0..3 {
            let started = std::time::Instant::now();
            let sample = sampler.sample(1.0);
            quickest = quickest.min(started.elapsed());
            assert!(!sample.adapters.is_empty(), "no hardware adapter is up");
            assert!(sample.adapters.iter().all(|a| !a.name.is_empty()));
        }
        assert!(quickest.as_millis() < 200, "GetIfTable2 took {quickest:?}");
    }
}
