// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Cutting a program off the network with Windows Firewall: two rules, in
//! and out, that block its executable, named so they are Anabolic's and
//! easy to find in Windows' own firewall settings too. Removing them gives
//! the program its network back. Needs administrator rights.

use windows::Win32::Foundation::VARIANT_TRUE;
use windows::Win32::NetworkManagement::WindowsFirewall::{
    INetFwPolicy2, INetFwRule, NET_FW_ACTION_BLOCK, NET_FW_PROFILE2_ALL, NET_FW_RULE_DIR_IN,
    NET_FW_RULE_DIR_OUT, NetFwPolicy2, NetFwRule,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::BSTR;

use crate::util::error_text;

const GROUP: &str = "Anabolic";

/// The two rules' names for a program.
fn names(path: &str) -> [String; 2] {
    let file = std::path::Path::new(path)
        .file_name()
        .map_or(path.to_string(), |f| f.to_string_lossy().into_owned());
    [
        format!("Anabolic: block {file} (out) {path}"),
        format!("Anabolic: block {file} (in) {path}"),
    ]
}

fn policy() -> windows::core::Result<INetFwPolicy2> {
    unsafe { CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER) }
}

/// Blocks `path` in both directions; needs COM on the calling thread.
pub fn block(path: &str) -> Result<(), String> {
    let run = || -> windows::core::Result<()> {
        let rules = unsafe { policy()?.Rules()? };
        let [out, inbound] = names(path);
        for (name, direction) in [(out, NET_FW_RULE_DIR_OUT), (inbound, NET_FW_RULE_DIR_IN)] {
            unsafe {
                // Again on top of an old one would make two.
                let _ = rules.Remove(&BSTR::from(name.as_str()));
                let rule: INetFwRule = CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER)?;
                rule.SetName(&BSTR::from(name.as_str()))?;
                rule.SetDescription(&BSTR::from(
                    "Added by Anabolic (Block network access). Remove it there, or delete it here.",
                ))?;
                rule.SetGrouping(&BSTR::from(GROUP))?;
                rule.SetApplicationName(&BSTR::from(path))?;
                rule.SetDirection(direction)?;
                rule.SetAction(NET_FW_ACTION_BLOCK)?;
                rule.SetProfiles(NET_FW_PROFILE2_ALL.0)?;
                rule.SetEnabled(VARIANT_TRUE)?;
                rules.Add(&rule)?;
            }
        }
        Ok(())
    };
    run().map_err(|e| error_text(&e))
}

/// Removes the rules `block` made.
pub fn unblock(path: &str) -> Result<(), String> {
    let run = || -> windows::core::Result<()> {
        let rules = unsafe { policy()?.Rules()? };
        for name in names(path) {
            unsafe { rules.Remove(&BSTR::from(name.as_str()))? };
        }
        Ok(())
    };
    run().map_err(|e| error_text(&e))
}

/// Which of `paths` still have their rules (someone may have deleted them
/// in Windows' firewall settings).
pub fn still_blocked(paths: &[String]) -> Vec<String> {
    let Ok(rules) = (unsafe { policy().and_then(|p| p.Rules()) }) else {
        return paths.to_vec();
    };
    paths
        .iter()
        .filter(|path| {
            let [out, _] = names(path);
            unsafe { rules.Item(&BSTR::from(out.as_str())) }.is_ok()
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::names;

    #[test]
    fn names_carry_the_file_and_the_path() {
        let [out, inbound] = names(r"C:\Games\game.exe");
        assert_eq!(out, r"Anabolic: block game.exe (out) C:\Games\game.exe");
        assert!(inbound.contains("(in)"));
    }
}
