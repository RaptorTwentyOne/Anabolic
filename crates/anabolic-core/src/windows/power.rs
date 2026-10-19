//! Windows power plans: which one is active, and switching to High
//! performance while a game runs (and back afterwards). Calls go to the
//! power service, so they run on the action thread.

use windows::Win32::Foundation::{ERROR_SUCCESS, HLOCAL, LocalFree};
use windows::Win32::System::Power::{
    PowerGetActiveScheme, PowerReadFriendlyName, PowerSetActiveScheme,
};
use windows::Win32::System::SystemServices::GUID_MIN_POWER_SAVINGS;
use windows::core::GUID;

use crate::util::{error_text, from_wide};

/// Ultimate Performance; hidden unless someone added it.
const ULTIMATE: GUID = GUID::from_u128(0xe9a42b02_d5df_448d_aa00_03f14749eb61);

pub fn active() -> Option<GUID> {
    let mut at: *mut GUID = std::ptr::null_mut();
    unsafe {
        if PowerGetActiveScheme(None, &mut at) != ERROR_SUCCESS || at.is_null() {
            return None;
        }
        let guid = *at;
        let _ = LocalFree(Some(HLOCAL(at.cast())));
        Some(guid)
    }
}

pub fn name(scheme: &GUID) -> String {
    let mut size = 0u32;
    unsafe {
        let _ = PowerReadFriendlyName(None, Some(scheme), None, None, None, &mut size);
        if size == 0 {
            return format!("{scheme:?}");
        }
        let mut buf = vec![0u16; size as usize / 2 + 1];
        if PowerReadFriendlyName(
            None,
            Some(scheme),
            None,
            None,
            Some(buf.as_mut_ptr().cast()),
            &mut size,
        ) != ERROR_SUCCESS
        {
            return format!("{scheme:?}");
        }
        from_wide(&buf)
    }
}

fn set(scheme: &GUID) -> Result<(), String> {
    let err = unsafe { PowerSetActiveScheme(None, Some(scheme)) };
    if err == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(error_text(&windows::core::Error::from(err.to_hresult())))
    }
}

/// Whether a plan already runs the processor flat out.
fn fast(scheme: &GUID) -> bool {
    *scheme == GUID_MIN_POWER_SAVINGS || *scheme == ULTIMATE
}

/// Switches to High performance. Returns the plan it replaced, or None
/// when a fast plan was already active (nothing to undo).
pub fn for_game() -> Result<Option<(GUID, String)>, String> {
    let now = active().ok_or("the active power plan could not be read")?;
    if fast(&now) {
        return Ok(None);
    }
    set(&GUID_MIN_POWER_SAVINGS)?;
    Ok(Some((now, name(&now))))
}

/// Puts `previous` back, unless someone picked another plan meanwhile.
pub fn restore(previous: &GUID) -> Result<Option<String>, String> {
    match active() {
        Some(now) if now == GUID_MIN_POWER_SAVINGS => {
            set(previous)?;
            Ok(Some(name(previous)))
        }
        _ => Ok(None),
    }
}

pub fn parse(text: &str) -> Option<GUID> {
    GUID::try_from(text.trim_matches(|c| c == '{' || c == '}')).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_active_plan() {
        let now = active().expect("a plan is active");
        assert!(!name(&now).is_empty());
        let text = format!("{now:?}");
        assert_eq!(parse(&text), Some(now));
    }
}
