// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Starting the program to record as the signed-in user, through Explorer,
//! and knowing it when it appears.

/// The image name the kernel will report for a program: its file name,
/// with ".exe" when it has no extension.
pub(super) fn expected_image(program: &str) -> String {
    let file = std::path::Path::new(program)
        .file_name()
        .map_or(program.to_string(), |f| f.to_string_lossy().into_owned());
    let file = if file.contains('.') {
        file
    } else {
        format!("{file}.exe")
    };
    file.to_ascii_lowercase()
}

/// Whether the kernel's (possibly cut) image name is `expected`.
pub(super) fn image_matches(reported: &str, expected: &str) -> bool {
    let reported = reported.to_ascii_lowercase();
    !reported.is_empty()
        && (reported == expected || (reported.len() >= 14 && expected.starts_with(&reported)))
}

// ---------------------------------------------------------------------------
// The consumer: everything below runs on the trace's consumer thread.

/// The signed-in user's SID ("S-1-5-21-..."), to show HKCU as HKCU.
pub(super) use crate::util::user_sid;

/// Starts a program as the signed-in user through Explorer's own shell
/// window (the documented way for an elevated program to start an
/// unelevated one).
pub(super) fn shell_launch(
    program: &str,
    args: &str,
    dir: Option<&str>,
    show: i32,
) -> Result<(), String> {
    use windows::Win32::System::Com::{
        CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IDispatch,
        IServiceProvider,
    };
    use windows::Win32::System::Variant::{VARIANT, VT_BSTR, VT_I4, VariantClear};
    use windows::Win32::UI::Shell::{
        IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellView, IShellWindows,
        SID_STopLevelBrowser, SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH, ShellWindows,
    };
    use windows::core::{BSTR, Interface};

    fn int(value: i32) -> VARIANT {
        let mut v = VARIANT::default();
        unsafe {
            (*v.Anonymous.Anonymous).vt = VT_I4;
            (*v.Anonymous.Anonymous).Anonymous.lVal = value;
        }
        v
    }
    fn text(value: &str) -> VARIANT {
        let mut v = VARIANT::default();
        unsafe {
            (*v.Anonymous.Anonymous).vt = VT_BSTR;
            (*v.Anonymous.Anonymous).Anonymous.bstrVal =
                std::mem::ManuallyDrop::new(BSTR::from(value));
        }
        v
    }
    let run = || -> windows::core::Result<()> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let windows: IShellWindows =
                CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)?;
            let (location, root) = (int(0), VARIANT::default());
            let mut hwnd = 0i32;
            let desktop = windows.FindWindowSW(
                &location,
                &root,
                SWC_DESKTOP,
                &mut hwnd,
                SWFO_NEEDDISPATCH,
            )?;
            let provider: IServiceProvider = desktop.cast()?;
            let browser: IShellBrowser = provider.QueryService(&SID_STopLevelBrowser)?;
            let view: IShellView = browser.QueryActiveShellView()?;
            let background: IDispatch = view.GetItemObject(SVGIO_BACKGROUND)?;
            let folder: IShellFolderViewDual = background.cast()?;
            let shell: IShellDispatch2 = folder.Application()?.cast()?;
            let (mut a, mut d, mut op, mut s) =
                (text(args), text(dir.unwrap_or("")), text(""), int(show));
            let result = shell.ShellExecute(&BSTR::from(program), &a, &d, &op, &s);
            for v in [&mut a, &mut d, &mut op, &mut s] {
                let _ = VariantClear(v);
            }
            result
        }
    };
    run().map_err(|e| crate::util::error_text(&e))
}
