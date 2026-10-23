// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What differs between Windows and Linux in what the pages say: the
//! system's name, what "elevated" is called and how it is had, and the
//! processes nothing should touch.

#[cfg(windows)]
mod imp {
    pub const SYSTEM: &str = "Windows";

    /// Running with every right Anabolic asks for.
    pub const ELEVATED: &str = "Administrator";
    pub const ELEVATED_TIP: &str =
        "Running elevated: every process, disk counters and owners are visible.";
    pub const RUNNING_ELEVATED: &str = "Running as administrator";
    pub const RUNNING_PLAIN: &str = "Running without administrator rights";
    /// The button that gets them.
    pub const ELEVATE: &str = "Restart as admin";
    pub const PLAIN_TIP: &str =
        "Without elevation protected processes, disk counters and some owners are hidden.";
    pub const ELEVATING: &str = "Waiting for the UAC prompt.";
    /// What to do when something needed them.
    pub const ELEVATE_AND_RETRY: &str =
        "Restart Anabolic as administrator (top right) and try again.";
    pub const ELEVATED_READS_MORE: &str = "Running Anabolic as administrator reads most of these.";
    pub const NEEDS_ELEVATION: &str = "Needs Anabolic running as administrator";
    /// A process started with them.
    pub const STARTED_ELEVATED: &str = "as administrator";

    pub const SHOW_IN_FILE_MANAGER: &str = "Show in Explorer";

    /// Never lowered by game mode: Windows' core, the shell, audio
    /// (lowering it makes sound crackle), and Anabolic.
    pub const CORE_PROCESSES: &[&str] = &[
        "system",
        "registry",
        "secure system",
        "memory compression",
        "smss.exe",
        "csrss.exe",
        "wininit.exe",
        "winlogon.exe",
        "services.exe",
        "lsass.exe",
        "lsaiso.exe",
        "dwm.exe",
        "audiodg.exe",
        "fontdrvhost.exe",
        "explorer.exe",
        "sihost.exe",
        "ctfmon.exe",
        "anabolic.exe",
    ];

    /// Ending or suspending these takes Windows down or signs the user out.
    pub const CRITICAL_PROCESSES: &[&str] = &[
        "system",
        "registry",
        "secure system",
        "memory compression",
        "smss.exe",
        "csrss.exe",
        "wininit.exe",
        "winlogon.exe",
        "services.exe",
        "lsass.exe",
        "lsaiso.exe",
    ];
}

#[cfg(target_os = "linux")]
mod imp {
    pub const SYSTEM: &str = "Linux";

    /// Holding every capability Anabolic asks for (or running as root).
    pub const ELEVATED: &str = "Full access";
    pub const ELEVATED_TIP: &str = "Every process is visible with its open files, threads and \
                                    disk traffic, and kernel traces can run.";
    pub const RUNNING_ELEVATED: &str = "Running with full access";
    pub const RUNNING_PLAIN: &str = "Running without full access";
    /// The button that gets them: the executable is given capabilities
    /// (pkexec asks for the password once).
    pub const ELEVATE: &str = "Grant full access";
    pub const PLAIN_TIP: &str = "Without full access other users' processes keep their open \
                                 files, threads and disk traffic to themselves, and kernel traces \
                                 cannot run. Granting it gives this executable the capabilities \
                                 it needs (pkexec asks for your password once).";
    pub const ELEVATING: &str = "Waiting for the password prompt.";
    pub const ELEVATE_AND_RETRY: &str = "Give Anabolic full access (top right) and try again.";
    pub const ELEVATED_READS_MORE: &str = "With full access Anabolic reads most of these.";
    pub const NEEDS_ELEVATION: &str = "Needs Anabolic to have full access";
    pub const STARTED_ELEVATED: &str = "as root";

    pub const SHOW_IN_FILE_MANAGER: &str = "Show in file manager";

    /// Never lowered by game mode: the system's core, the desktop (its
    /// compositor draws the game), audio (lowering it makes sound
    /// crackle), and Anabolic.
    pub const CORE_PROCESSES: &[&str] = &[
        "kernel",
        "systemd",
        "systemd-journald",
        "systemd-logind",
        "systemd-udevd",
        "dbus-daemon",
        "dbus-broker",
        "dbus-broker-launch",
        "polkitd",
        "xorg",
        "xwayland",
        "kwin_wayland",
        "kwin_x11",
        "plasmashell",
        "gnome-shell",
        "mutter",
        "pipewire",
        "pipewire-pulse",
        "wireplumber",
        "pulseaudio",
        "anabolic",
    ];

    /// Ending or suspending these takes the system down or ends the
    /// session: init, the kernel, the session's manager and its compositor
    /// (on Wayland every window goes with it).
    pub const CRITICAL_PROCESSES: &[&str] = &[
        "kernel",
        "systemd",
        "systemd-journald",
        "systemd-logind",
        "dbus-daemon",
        "dbus-broker",
        "sddm",
        "gdm",
        "kwin_wayland",
        "gnome-shell",
        "xorg",
    ];
}

pub use imp::*;

/// The processes Windows or Linux itself runs before anything else: the
/// kernel's and init (Windows' System, PID 4; Linux's init and the
/// kernel's threads, PIDs 1 and 2).
pub fn system_pid(pid: u32) -> bool {
    pid <= anabolic_core::scan::KERNEL_PID
}
