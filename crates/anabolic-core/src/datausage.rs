// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What each program sent and received over the last weeks, in the
//! manner of NetworkUsageView: Windows' own record (the System Resource
//! Usage Monitor, which Settings' Data usage page reads), kept for about
//! 60 days and from before Anabolic was installed. Read through the
//! network connection profiles' attributed usage, which needs no
//! administrator. Linux keeps no such record.

use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppUsage {
    /// The program's path, or a Store app's package name.
    pub id: String,
    /// Its name as Windows gives it.
    pub name: String,
    pub sent: u64,
    pub received: u64,
}

impl AppUsage {
    pub fn total(&self) -> u64 {
        self.sent + self.received
    }
}

#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub days: u32,
    /// Most first.
    pub apps: Vec<AppUsage>,
    /// How many networks (connection profiles) it covers.
    pub networks: usize,
    pub took: Duration,
}

impl Usage {
    pub fn total(&self) -> (u64, u64) {
        (
            self.apps.iter().map(|a| a.sent).sum(),
            self.apps.iter().map(|a| a.received).sum(),
        )
    }
}

/// Adds one network's figures to the totals, by program.
pub fn add(into: &mut Vec<AppUsage>, id: &str, name: &str, sent: u64, received: u64) {
    let key = id.to_lowercase();
    match into.iter_mut().find(|a| a.id.to_lowercase() == key) {
        Some(a) => {
            a.sent += sent;
            a.received += received;
        }
        None => into.push(AppUsage {
            id: id.to_string(),
            name: if name.is_empty() && id.is_empty() {
                "Not tied to a program".to_string()
            } else if name.is_empty() {
                id.rsplit(['\\', '/']).next().unwrap_or(id).to_string()
            } else {
                name.to_string()
            },
            sent,
            received,
        }),
    }
}

#[cfg(windows)]
pub fn read(days: u32) -> Result<Usage, String> {
    use windows::Foundation::DateTime;
    use windows::Networking::Connectivity::{NetworkInformation, NetworkUsageStates, TriStates};
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

    let started = std::time::Instant::now();
    // WinRT on this thread.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let now = crate::sysinfo::now_filetime();
    let start = DateTime {
        UniversalTime: now - days as i64 * 86_400 * 10_000_000,
    };
    let end = DateTime { UniversalTime: now };
    let states = NetworkUsageStates {
        Roaming: TriStates::DoNotCare,
        Shared: TriStates::DoNotCare,
    };
    let profiles = NetworkInformation::GetConnectionProfiles().map_err(|e| e.message())?;
    // Every network's query started first, then waited for: they run
    // together, where one after another takes a minute.
    let queries: Vec<_> = profiles
        .into_iter()
        .filter_map(|p| p.GetAttributedNetworkUsageAsync(start, end, states).ok())
        .collect();
    let map = crate::winfile::DeviceMap::build();
    let mut apps = Vec::new();
    let mut networks = 0;
    for op in queries {
        let Ok(list) = op.join() else {
            continue;
        };
        networks += 1;
        for u in list {
            let id = u.AttributionId().map(|s| s.to_string()).unwrap_or_default();
            let id = map.to_dos_path(&id).unwrap_or(id);
            let name = u
                .AttributionName()
                .map(|s| s.to_string())
                .unwrap_or_default();
            add(
                &mut apps,
                &id,
                &name,
                u.BytesSent().unwrap_or(0),
                u.BytesReceived().unwrap_or(0),
            );
        }
    }
    apps.sort_by_key(|a| std::cmp::Reverse(a.total()));
    Ok(Usage {
        days,
        apps,
        networks,
        took: started.elapsed(),
    })
}

#[cfg(target_os = "linux")]
pub fn read(_days: u32) -> Result<Usage, String> {
    Err(
        "Linux keeps no record of what each program sent; Anabolic's own History has the last \
week's."
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_up_by_program() {
        let mut v = Vec::new();
        add(&mut v, r"C:\Apps\Game.exe", "", 10, 20);
        add(&mut v, r"c:\apps\game.exe", "Game", 1, 2);
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].sent, v[0].received), (11, 22));
        assert_eq!(v[0].name, "Game.exe");
    }

    #[cfg(windows)]
    #[test]
    fn reads_the_last_month() {
        let u = read(30).unwrap();
        let (sent, received) = u.total();
        println!(
            "{} programs on {} networks, {} MB sent, {} MB received, in {:?}",
            u.apps.len(),
            u.networks,
            sent >> 20,
            received >> 20,
            u.took
        );
        for a in u.apps.iter().take(8) {
            println!(
                "{:<40} {:>8} MB down {:>8} MB up  {}",
                a.name,
                a.received >> 20,
                a.sent >> 20,
                a.id
            );
        }
    }
}
