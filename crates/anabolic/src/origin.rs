//! "Started by": the chain of processes that led to this one, and in plain
//! words how it was most likely started (from the shell, by a service, a
//! scheduled task, COM, WMI, a script...), read from who its parents were.

use std::collections::HashMap;
use std::sync::Arc;

use anabolic_core::lineage::Ancestor;
use anabolic_core::services::ServiceList;
use anabolic_core::sysinfo::local_time;
use anabolic_core::{ProcKey, ProcState, ProcessInfo, Snapshot};
use eframe::egui::{Label, RichText, Ui};

use crate::proc_table::UiCmd;
use crate::{theme, widgets};

/// The services a process runs, by service name.
fn services_of(list: Option<&ServiceList>, pid: u32) -> Vec<&str> {
    list.map(|l| {
        l.services
            .iter()
            .filter(|s| s.pid == pid && pid != 0)
            .map(|s| &*s.name)
            .collect()
    })
    .unwrap_or_default()
}

/// How the process was most likely started, from its parent (and the
/// parent's parent when that says more).
pub fn how(name: &str, pid: u32, chain: &[Ancestor], services: Option<&ServiceList>) -> String {
    let Some(parent) = chain.first() else {
        return String::new();
    };
    let lower = |s: &str| s.to_lowercase();
    let parent_name = lower(&parent.name);
    let hosted = services_of(services, parent.key.pid);
    let hosts = |service: &str| hosted.iter().any(|s| s.eq_ignore_ascii_case(service));
    match parent_name.as_str() {
        "explorer.exe" => "From Windows Explorer: the Start menu, the taskbar, a shortcut, a \
                           folder or the Run box."
            .into(),
        "services.exe" => match services_of(services, pid).as_slice() {
            [] => "By Windows as a service (the Service Control Manager starts them).".into(),
            mine => format!("By Windows as a service: {}.", mine.join(", ")),
        },
        "svchost.exe" if hosts("Schedule") => "By Task Scheduler: a scheduled task ran it.".into(),
        "svchost.exe" if hosts("DcomLaunch") => {
            "By COM (DCOM Server Process Launcher), because another program asked for it.".into()
        }
        "svchost.exe" if hosts("BrokerInfrastructure") || hosts("AppXSvc") => {
            "By Windows for an app (Store or system app).".into()
        }
        "svchost.exe" if hosts("Winmgmt") => "Through WMI.".into(),
        "svchost.exe" if !hosted.is_empty() => {
            format!("By a Windows service host ({}).", hosted.join(", "))
        }
        "wmiprvse.exe" => "Through WMI: a script or a management tool asked for it.".into(),
        "cmd.exe"
        | "powershell.exe"
        | "pwsh.exe"
        | "bash.exe"
        | "wsl.exe"
        | "conhost.exe"
        | "windowsterminal.exe"
        | "openconsole.exe" => "From a command line or a script.".into(),
        "userinit.exe" | "winlogon.exe" => "When you signed in.".into(),
        "smss.exe" | "wininit.exe" | "system" => "By Windows while it started.".into(),
        "msiexec.exe" => "By Windows Installer (a setup).".into(),
        "sihost.exe" | "startmenuexperiencehost.exe" | "searchhost.exe" => {
            "From the Start menu or search.".into()
        }
        "runtimebroker.exe" | "applicationframehost.exe" => "By Windows for an app.".into(),
        p if p == lower(name) => format!(
            "By another {} process: programs like this start helpers of their own.",
            parent.name
        ),
        _ => format!("By {}.", parent.name),
    }
}

fn exit_text(code: u32) -> String {
    match code {
        0 => "exit code 0".into(),
        0xC000_0005 => "crashed (access violation)".into(),
        0xC000_0409 => "crashed (stack buffer overrun / fail fast)".into(),
        0xC000_013A => "stopped with Ctrl+C".into(),
        0xC000_00FD => "crashed (stack overflow)".into(),
        c if c > 0xFFFF => format!("exit code 0x{c:08X}"),
        c => format!("exit code {c}"),
    }
}

fn when(filetime: i64) -> String {
    local_time(filetime).unwrap_or_default()
}

/// The "Started by" block of the General tab.
pub fn show(
    ui: &mut Ui,
    p: &ProcessInfo,
    snap: &Snapshot,
    cmdlines: &HashMap<ProcKey, Arc<str>>,
    services: Option<&ServiceList>,
    out: &mut Vec<UiCmd>,
) {
    let Some(sel) = snap.selected.as_ref().filter(|s| s.key == p.key) else {
        return;
    };
    let chain = &sel.ancestry;
    ui.add_space(10.0);
    ui.label(widgets::strong("Started by", theme::BODY));
    ui.add_space(2.0);
    if chain.is_empty() {
        let text = match p.ppid {
            0 => "Nothing: Windows itself.".to_string(),
            ppid => format!(
                "Its parent (PID {ppid}) had already exited when Anabolic started, so who it \
                 was is not known. Anabolic remembers parents from the moment it runs."
            ),
        };
        ui.label(RichText::new(text).color(theme::TEXT_WEAK));
        return;
    }
    let how = how(&p.name, p.pid, chain, services);
    if !how.is_empty() {
        ui.label(RichText::new(how).color(theme::TEXT));
        ui.add_space(4.0);
    }
    for (depth, a) in chain.iter().enumerate() {
        let alive = a.exited.is_none()
            && snap
                .find(a.key)
                .is_some_and(|q| q.state != ProcState::Exited);
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * 14.0);
            ui.label(RichText::new("↳").color(theme::TEXT_DIM));
            let title = format!("{} ({})", a.name, a.key.pid);
            if alive {
                if widgets::link(ui, &title).clicked() {
                    out.push(UiCmd::Select(Some(a.key)));
                }
            } else {
                ui.label(RichText::new(title).color(theme::TEXT_WEAK));
            }
            let mut facts = vec![format!("started {}", when(a.key.create_time))];
            match a.exited {
                Some((at, code)) => {
                    let mut gone = format!("exited {}", when(at));
                    if let Some(code) = code {
                        gone.push_str(&format!(", {}", exit_text(code)));
                    }
                    facts.push(gone);
                }
                None if alive => facts.push("running".into()),
                None => {}
            }
            if a.elevated == Some(true) {
                facts.push("as administrator".into());
            }
            ui.label(
                RichText::new(facts.join("  ·  "))
                    .size(theme::META)
                    .color(theme::TEXT_DIM),
            );
        });
        let detail = cmdlines
            .get(&a.key)
            .map(|c| c.to_string())
            .or_else(|| a.image.as_ref().map(|i| i.to_string()));
        if let Some(detail) = detail {
            ui.horizontal(|ui| {
                ui.add_space(depth as f32 * 14.0 + 18.0);
                ui.add(
                    Label::new(
                        RichText::new(detail)
                            .size(theme::META)
                            .monospace()
                            .color(theme::TEXT_DIM),
                    )
                    .truncate(),
                );
            });
        }
    }
    if let Some(last) = chain.last()
        && last.ppid != 0
        && chain.len() < 32
    {
        let known_root = matches!(
            last.name.to_lowercase().as_str(),
            "system" | "smss.exe" | "wininit.exe" | "services.exe"
        );
        if !known_root {
            ui.label(
                RichText::new(format!(
                    "Before that: PID {}, gone before Anabolic started.",
                    last.ppid
                ))
                .size(theme::META)
                .color(theme::TEXT_DIM),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ancestor(pid: u32, name: &str) -> Ancestor {
        Ancestor {
            key: ProcKey {
                pid,
                create_time: 1,
            },
            name: name.into(),
            image: None,
            ppid: 0,
            exited: None,
            elevated: None,
        }
    }

    #[test]
    fn words_the_usual_parents() {
        assert!(how("app.exe", 7, &[ancestor(1, "explorer.exe")], None).contains("Explorer"));
        assert!(how("app.exe", 7, &[ancestor(1, "cmd.exe")], None).contains("command line"));
        assert!(how("chrome.exe", 7, &[ancestor(1, "chrome.exe")], None).contains("helpers"));
        assert!(how("x.exe", 7, &[ancestor(1, "launcher.exe")], None).contains("launcher.exe"));
        assert_eq!(how("x.exe", 7, &[], None), "");
    }
}
