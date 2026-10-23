// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The monitors: which one is which (its own name, not \\.\DISPLAY3), the
//! GPU each hangs off, its mode and the fastest refresh it offers, HDR and
//! scaling. A monitor on the integrated GPU, or one running at 60 Hz that
//! could do 144, is a stutter nobody sees in the game's settings.

use std::collections::HashMap;

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EXTERNAL,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_USB_TUNNEL, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DVI,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HD15, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_VIRTUAL,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_WIRED, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_MIRACAST, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{LUID, POINT};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
use windows::Win32::Graphics::Gdi::{
    DEVMODEW, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplaySettingsW, MONITOR_DEFAULTTONEAREST,
    MonitorFromPoint,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::core::PCWSTR;

use crate::util::{from_wide, wide};

use super::*;

fn connection(t: DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY) -> &'static str {
    match t {
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EXTERNAL => "DisplayPort",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_USB_TUNNEL => "DisplayPort over USB-C",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI => "HDMI",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DVI => "DVI",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HD15 => "VGA",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL
        | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED
        | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED => "built in",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_MIRACAST => "wireless (Miracast)",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_VIRTUAL
        | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_WIRED => "virtual",
        _ => "other",
    }
}

/// GPU names by adapter LUID.
fn gpus() -> HashMap<(u32, i32), String> {
    let mut map = HashMap::new();
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return map;
    };
    for i in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else {
            break;
        };
        if let Ok(d) = unsafe { adapter.GetDesc1() } {
            map.insert(
                (d.AdapterLuid.LowPart, d.AdapterLuid.HighPart),
                from_wide(&d.Description),
            );
        }
    }
    map
}

/// The fastest refresh `device` offers at `width` x `height`, at all, and
/// its largest mode.
fn fastest(device: &[u16], width: u32, height: u32) -> (u32, u32, (u32, u32)) {
    let (mut here, mut any) = (0, 0);
    let mut native = (0u32, 0u32);
    for i in 0.. {
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        if !unsafe {
            EnumDisplaySettingsW(
                PCWSTR(device.as_ptr()),
                ENUM_DISPLAY_SETTINGS_MODE(i),
                &mut mode,
            )
        }
        .as_bool()
        {
            break;
        }
        any = any.max(mode.dmDisplayFrequency);
        if mode.dmPelsWidth == width && mode.dmPelsHeight == height {
            here = here.max(mode.dmDisplayFrequency);
        }
        let pixels = |(w, h): (u32, u32)| w as u64 * h as u64;
        if pixels((mode.dmPelsWidth, mode.dmPelsHeight)) > pixels(native) {
            native = (mode.dmPelsWidth, mode.dmPelsHeight);
        }
    }
    (here, any, native)
}

fn header<T>(
    kind: windows::Win32::Devices::Display::DISPLAYCONFIG_DEVICE_INFO_TYPE,
    adapter: LUID,
    id: u32,
) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: kind,
        size: size_of::<T>() as u32,
        adapterId: adapter,
        id,
    }
}

/// Every active monitor.
pub fn displays() -> Vec<Display> {
    let (mut paths_n, mut modes_n) = (0u32, 0u32);
    if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths_n, &mut modes_n) }.0
        != 0
    {
        return Vec::new();
    }
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); paths_n as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); modes_n as usize];
    if unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut paths_n,
            paths.as_mut_ptr(),
            &mut modes_n,
            modes.as_mut_ptr(),
            None,
        )
    }
    .0 != 0
    {
        return Vec::new();
    }
    paths.truncate(paths_n as usize);
    modes.truncate(modes_n as usize);
    let gpus = gpus();
    let mut out = Vec::new();
    for path in &paths {
        let source = path.sourceInfo;
        let target = path.targetInfo;
        let mut source_name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: header::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(
                DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                source.adapterId,
                source.id,
            ),
            ..Default::default()
        };
        if unsafe { DisplayConfigGetDeviceInfo(&mut source_name.header) } != 0 {
            continue;
        }
        let device = from_wide(&source_name.viewGdiDeviceName);
        let mut target_name = DISPLAYCONFIG_TARGET_DEVICE_NAME {
            header: header::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(
                DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                target.adapterId,
                target.id,
            ),
            ..Default::default()
        };
        let name = if unsafe { DisplayConfigGetDeviceInfo(&mut target_name.header) } == 0 {
            from_wide(&target_name.monitorFriendlyDeviceName)
        } else {
            String::new()
        };
        let mut color = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
            header: header::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(
                DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                target.adapterId,
                target.id,
            ),
            ..Default::default()
        };
        let (hdr_supported, hdr_on, bits) =
            if unsafe { DisplayConfigGetDeviceInfo(&mut color.header) } == 0 {
                let bits = unsafe { color.Anonymous.value };
                (bits & 1 != 0, bits & 2 != 0, color.bitsPerColorChannel)
            } else {
                (false, false, 0)
            };
        let index = unsafe { source.Anonymous.modeInfoIdx } as usize;
        let Some(mode) = modes
            .get(index)
            .filter(|m| m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE)
        else {
            continue;
        };
        let src = unsafe { mode.Anonymous.sourceMode };
        let rate = target.refreshRate;
        let refresh_hz = if rate.Denominator == 0 {
            0.0
        } else {
            rate.Numerator as f64 / rate.Denominator as f64
        };
        let device_w = wide(&device);
        let (max_hz_here, max_hz, native) = fastest(&device_w, src.width, src.height);
        let centre = POINT {
            x: src.position.x + src.width as i32 / 2,
            y: src.position.y + src.height as i32 / 2,
        };
        let monitor = unsafe { MonitorFromPoint(centre, MONITOR_DEFAULTTONEAREST) };
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
        out.push(Display {
            device,
            name,
            gpu: gpus
                .get(&(source.adapterId.LowPart, source.adapterId.HighPart))
                .cloned()
                .unwrap_or_default(),
            connection: connection(target.outputTechnology),
            primary: src.position.x == 0 && src.position.y == 0,
            x: src.position.x,
            y: src.position.y,
            width: src.width,
            height: src.height,
            refresh_hz,
            max_hz_here,
            max_hz,
            native,
            hdr_supported,
            hdr_on,
            bits_per_color: bits,
            scale_pct: dpi_x * 100 / 96,
        });
    }
    out.sort_by_key(|d| (d.x, d.y));
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_the_monitors() {
        let all = super::displays();
        assert!(!all.is_empty());
        assert_eq!(all.iter().filter(|d| d.primary).count(), 1, "{all:#?}");
        for d in &all {
            assert!(d.device.starts_with(r"\\.\DISPLAY"), "{d:?}");
            assert!(d.width > 0 && d.refresh_hz > 0.0, "{d:?}");
        }
    }
}
