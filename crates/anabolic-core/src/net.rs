//! System-wide network throughput, per physical adapter.

use std::collections::HashMap;

use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;

use crate::util::from_wide;

#[derive(Clone, Debug, Default)]
pub struct AdapterStats {
    /// Stable identity (interface LUID).
    pub luid: u64,
    pub name: String,
    pub description: String,
    /// Bits per second, as negotiated.
    pub link_speed: u64,
    pub rx_rate: f64,
    pub tx_rate: f64,
    pub rx_total: u64,
    pub tx_total: u64,
}

#[derive(Clone, Debug, Default)]
pub struct NetStats {
    pub rx_rate: f64,
    pub tx_rate: f64,
    pub adapters: Vec<AdapterStats>,
}

#[derive(Default)]
pub struct NetSampler {
    prev: HashMap<u64, (u64, u64)>,
}

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
        let started = std::time::Instant::now();
        let first = sampler.sample(1.0);
        let took = started.elapsed();
        assert!(!first.adapters.is_empty(), "no hardware adapter is up");
        assert!(took.as_millis() < 200, "GetIfTable2 took {took:?}");
        let second = sampler.sample(1.0);
        assert!(second.adapters.iter().all(|a| !a.name.is_empty()));
    }
}
