// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Wi-Fi on Windows through the Native Wifi API: the adapters, each access
//! point the last scan heard (WlanGetNetworkBssList), the networks' security
//! (WlanGetAvailableNetworkList) and the connection (its current
//! connection and channel). Since Windows 11 24H2 the access points are
//! given only to programs allowed the location (Settings > Privacy >
//! Location > Let desktop apps access your location).

use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, HANDLE};
use windows::Win32::NetworkManagement::WiFi::{
    DOT11_PHY_TYPE, WLAN_AVAILABLE_NETWORK, WLAN_AVAILABLE_NETWORK_LIST, WLAN_BSS_ENTRY,
    WLAN_BSS_LIST, WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST, WLAN_RADIO_STATE,
    WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory, WlanGetAvailableNetworkList,
    WlanGetNetworkBssList, WlanOpenHandle, WlanQueryInterface, WlanScan, dot11_BSS_type_any,
    dot11_phy_type_eht, dot11_phy_type_erp, dot11_phy_type_he, dot11_phy_type_ht,
    dot11_phy_type_ofdm, dot11_phy_type_vht, dot11_radio_state_off,
    wlan_intf_opcode_channel_number, wlan_intf_opcode_current_connection,
    wlan_intf_opcode_radio_state,
};

use crate::util::from_wide;
use crate::wifi::{AccessPoint, Link, Wifi, channel};

struct Client(HANDLE);

impl Drop for Client {
    fn drop(&mut self) {
        unsafe { WlanCloseHandle(self.0, None) };
    }
}

fn generation(phy: DOT11_PHY_TYPE) -> String {
    match phy {
        p if p == dot11_phy_type_eht => "Wi-Fi 7",
        p if p == dot11_phy_type_he => "Wi-Fi 6",
        p if p == dot11_phy_type_vht => "Wi-Fi 5",
        p if p == dot11_phy_type_ht => "Wi-Fi 4",
        p if p == dot11_phy_type_erp || p == dot11_phy_type_ofdm => "802.11a/g",
        _ => "older",
    }
    .into()
}

fn security(auth: i32, secured: bool) -> String {
    match auth {
        1 if !secured => "Open",
        1 | 2 => "WEP",
        3 | 4 => "WPA",
        6 => "WPA2-Enterprise",
        7 => "WPA2",
        8 | 11 => "WPA3-Enterprise",
        9 => "WPA3",
        10 => "Enhanced Open",
        _ => "",
    }
    .into()
}

fn ssid_text(len: u32, bytes: &[u8; 32]) -> String {
    String::from_utf8_lossy(&bytes[..(len as usize).min(32)]).into_owned()
}

fn mac(b: &[u8; 6]) -> String {
    b.iter()
        .map(|x| format!("{x:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

pub fn read() -> Result<Wifi, String> {
    let mut version = 0u32;
    let mut handle = HANDLE::default();
    let err = unsafe { WlanOpenHandle(2, None, &mut version, &mut handle) };
    if err != 0 {
        return Err(
            "Windows' Wi-Fi service (WLAN AutoConfig) is not running, or there is no Wi-Fi \
adapter."
                .into(),
        );
    }
    let client = Client(handle);
    let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
    if unsafe { WlanEnumInterfaces(client.0, None, &mut list) } != 0 || list.is_null() {
        return Err("The Wi-Fi adapters could not be listed.".into());
    }
    let interfaces = unsafe {
        std::slice::from_raw_parts(
            (*list).InterfaceInfo.as_ptr(),
            (*list).dwNumberOfItems as usize,
        )
    }
    .to_vec();
    unsafe { WlanFreeMemory(list.cast()) };
    let Some(first) = interfaces.first() else {
        return Err("This PC has no Wi-Fi adapter.".into());
    };
    let guid = first.InterfaceGuid;
    let mut out = Wifi {
        adapter: from_wide(&first.strInterfaceDescription),
        ..Wifi::default()
    };
    // A radio turned off hears nothing.
    let mut size = 0u32;
    let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
    if unsafe {
        WlanQueryInterface(
            client.0,
            &guid,
            wlan_intf_opcode_radio_state,
            None,
            &mut size,
            &mut data,
            None,
        )
    } == 0
        && !data.is_null()
    {
        let radio = unsafe { &*data.cast::<WLAN_RADIO_STATE>() };
        let phys = &radio.PhyRadioState[..(radio.dwNumberOfPhys as usize).min(64)];
        let off = !phys.is_empty()
            && phys.iter().all(|p| {
                p.dot11SoftwareRadioState == dot11_radio_state_off
                    || p.dot11HardwareRadioState == dot11_radio_state_off
            });
        unsafe { WlanFreeMemory(data) };
        if off {
            out.note = Some(
                "The Wi-Fi radio is turned off (Settings > Network & internet > Wi-Fi, or the \
airplane mode): it hears no networks."
                    .into(),
            );
            return Ok(out);
        }
    }
    // Ask for a fresh scan; its results come within seconds, and the list
    // read now is the last one's.
    unsafe { WlanScan(client.0, &guid, None, None, None) };

    // Security, by network name.
    let mut secured: Vec<(String, String)> = Vec::new();
    let mut nets: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
    if unsafe { WlanGetAvailableNetworkList(client.0, &guid, 0, None, &mut nets) } == 0
        && !nets.is_null()
    {
        let items: &[WLAN_AVAILABLE_NETWORK] = unsafe {
            std::slice::from_raw_parts((*nets).Network.as_ptr(), (*nets).dwNumberOfItems as usize)
        };
        for n in items {
            secured.push((
                ssid_text(n.dot11Ssid.uSSIDLength, &n.dot11Ssid.ucSSID),
                security(n.dot11DefaultAuthAlgorithm.0, n.bSecurityEnabled.as_bool()),
            ));
        }
        unsafe { WlanFreeMemory(nets.cast()) };
    }

    // The connection.
    let mut size = 0u32;
    let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
    if unsafe {
        WlanQueryInterface(
            client.0,
            &guid,
            wlan_intf_opcode_current_connection,
            None,
            &mut size,
            &mut data,
            None,
        )
    } == 0
        && !data.is_null()
    {
        let c = unsafe { &*data.cast::<WLAN_CONNECTION_ATTRIBUTES>() };
        let a = &c.wlanAssociationAttributes;
        let mut ch_size = 0u32;
        let mut ch: *mut core::ffi::c_void = std::ptr::null_mut();
        let asked = unsafe {
            WlanQueryInterface(
                client.0,
                &guid,
                wlan_intf_opcode_channel_number,
                None,
                &mut ch_size,
                &mut ch,
                None,
            )
        };
        let number = if asked == 0 && !ch.is_null() {
            let n = unsafe { *ch.cast::<u32>() };
            unsafe { WlanFreeMemory(ch) };
            n
        } else {
            0
        };
        out.link = Some(Link {
            ssid: ssid_text(a.dot11Ssid.uSSIDLength, &a.dot11Ssid.ucSSID),
            bssid: mac(&a.dot11Bssid),
            band: None,
            channel: number,
            quality: a.wlanSignalQuality,
            rx_mbps: a.ulRxRate / 1000,
            tx_mbps: a.ulTxRate / 1000,
            generation: generation(a.dot11PhyType),
        });
        unsafe { WlanFreeMemory(data) };
    }

    // The access points.
    let mut bss: *mut WLAN_BSS_LIST = std::ptr::null_mut();
    let err = unsafe {
        WlanGetNetworkBssList(
            client.0,
            &guid,
            None,
            dot11_BSS_type_any,
            false,
            None,
            &mut bss,
        )
    };
    if err == ERROR_ACCESS_DENIED.0 {
        out.note = Some(
            "Windows gives the networks around only to programs allowed the location: \
Settings > Privacy & security > Location > Let desktop apps access your location."
                .into(),
        );
    } else if err == 0 && !bss.is_null() {
        let items: &[WLAN_BSS_ENTRY] = unsafe {
            std::slice::from_raw_parts(
                (*bss).wlanBssEntries.as_ptr(),
                (*bss).dwNumberOfItems as usize,
            )
        };
        for b in items {
            let ssid = ssid_text(b.dot11Ssid.uSSIDLength, &b.dot11Ssid.ucSSID);
            let bssid = mac(&b.dot11Bssid);
            let found = channel(b.ulChCenterFrequency / 1000);
            let connected = out.link.as_ref().is_some_and(|l| l.bssid == bssid);
            out.points.push(AccessPoint {
                security: secured
                    .iter()
                    .find(|(n, _)| *n == ssid)
                    .map(|(_, s)| s.clone())
                    .unwrap_or_default(),
                ssid,
                bssid,
                band: found.map(|f| f.0),
                channel: found.map_or(0, |f| f.1),
                dbm: b.lRssi,
                quality: b.uLinkQuality,
                generation: generation(b.dot11BssPhyType),
                connected,
            });
        }
        unsafe { WlanFreeMemory(bss.cast()) };
    }
    // The connection's band, from its access point.
    if let Some(link) = out.link.as_mut()
        && let Some(p) = out.points.iter().find(|p| p.bssid == link.bssid)
    {
        link.band = p.band;
        if link.channel == 0 {
            link.channel = p.channel;
        }
    }
    out.points.sort_by_key(|p| std::cmp::Reverse(p.dbm));
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_the_air() {
        match super::read() {
            Ok(w) => {
                println!("{} · link {:?} · note {:?}", w.adapter, w.link, w.note);
                for p in w.points.iter().take(10) {
                    println!(
                        "{:<24} {:?} ch {:>3} {:>4} dBm {:<8} {}",
                        p.ssid, p.band, p.channel, p.dbm, p.generation, p.security
                    );
                }
                println!("{:?}", crate::wifi::advice(&w));
            }
            Err(e) => println!("no Wi-Fi: {e}"),
        }
    }
}
