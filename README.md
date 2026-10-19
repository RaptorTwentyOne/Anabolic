<p align="center">
  <img src="crates/anabolic/assets/logo/lockup.svg" alt="Anabolic" width="420">
</p>

<h3 align="center">Task Manager on steroids</h3>

<p align="center">
  An open-source performance monitoring and diagnostics tool for Windows and Linux, built in Rust.<br>
  It stays responsive when the machine does not, and answers <em>who is doing this, and why</em>,<br>
  not only <em>how much</em>.
</p>

<p align="center">
  <a href="https://github.com/RaptorTwentyOne/Anabolic/actions/workflows/ci.yml"><img src="https://github.com/RaptorTwentyOne/Anabolic/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/RaptorTwentyOne/Anabolic/releases"><img src="https://img.shields.io/github/v/release/RaptorTwentyOne/Anabolic?include_prereleases&sort=semver" alt="Release"></a>
  <img src="https://img.shields.io/badge/platform-Windows%2010%20%7C%2011%20%C2%B7%20Linux-0078D4" alt="Windows 10 | 11 · Linux">
  <img src="https://img.shields.io/badge/rust-1.95%2B-orange" alt="Rust 1.95+">
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue" alt="MIT OR Apache-2.0"></a>
</p>

---

You open a task manager when something is wrong: a game stutters, the fans
spin up, a disk sits at 100 % and nothing says why. That is exactly when most
of them freeze, and when they do answer, they say *how much*, not *who* or
*why*. Anabolic is built for that moment:

- **It stays alive under load.** It raises its own priority, opts out of
  efficiency throttling, keeps its memory resident, never blocks its
  interface thread and reads the system once per refresh: about 1 ms of CPU
  per frame while scrolling. A running instance comes to the front in under
  100 ms.
- **It says who and why.** Who ran on each CPU core; which process reads and
  writes which file on each disk; traffic and GPU per program; which program
  started a process and how; what a hung thread waits for; which handles keep
  a dead process from going away.
- **It watches without touching.** Its regular views read system-wide tables
  and kernel traces: no process is opened, read or injected into, so a game
  is left alone while it is watched. Anything that does open a process asks
  first and says why.
- **It is honest about what it measures.** A value it cannot read is shown as
  unavailable, with the reason, never as zero.

![The Processes page: a Rust build in a terminal, with the compiler processes leading in CPU, memory and disk, next to VS Code, Chrome and Steam](docs/screenshots/processes.png)

## Contents

- [What it does](#what-it-does)
- [On Linux](#on-linux)
- [Getting started](#getting-started)
- [Keyboard](#keyboard)
- [AI assistants (MCP)](#ai-assistants-mcp)
- [Games and anti-cheat](#games-and-anti-cheat)
- [Privacy](#privacy)
- [Building from source](#building-from-source)
- [License](#license)

## What it does

| Page | |
|---|---|
| **Processes** | A tree, a flat list or grouped by app, with CPU, memory, disk, network, GPU, handles, user and command line. End, end tree, suspend, priority, cores, efficiency mode. Details show threads with their call stacks, modules with their signatures and hashes, handles in words, network connections, the services a process hosts and the chain of processes that started it, including parents that already exited. |
| **Performance** | CPU with who ran on each core, memory and its modules, each physical disk with its partitions and the processes and files it serves, network adapters, GPUs, process I/O, temperatures and fans, and every monitor on a map of the desktop. Windows lets only drivers read the processor's own temperature sensors, so there Anabolic shows the firmware's thermal zones, the GPU's temperature and the disks'. |
| **History** | Every 10 seconds, the machine's totals and the processes that led, kept for 7 days by default: a day strip, an hour graph and a range picker answer "what happened at 14:32". |
| **Rules** | Per program, in the manner of Process Lasso: priority, cores and efficiency mode applied to every new process, plus memory, CPU and handle alerts. A rule can mark a program as a game. |
| **Services** | Every service with its state, startup type, account and the process it runs in, with that process' CPU and memory; start, stop (asking about dependents), restart, pause, startup type. |
| **Startup** | Run keys, Startup folders and sign-in or boot tasks with their publisher and what they cost right now; turned on and off the way Task Manager does it. Nothing is deleted. |
| **Stutters** | Frame rate, 1 % lows and frame times of every program that draws, each stutter with what else happened at that moment, and DPC/ISR time per driver, in the manner of LatencyMon. No game is opened to measure it. |
| **Network** | Traffic per program, every connection with its state and the other end's name, the ports open to others and who can reach them, and every DNS lookup with the program that made it. Close a TCP connection, or block a program in Windows Firewall and allow it again. |
| **Activity** | Process Monitor style: every file, registry key, connection and program a process touches, in order, from a kernel trace. Start a program under recording, record a running one or its whole tree, or record the whole computer and filter afterwards. CSV export. |
| **MCP** | A Model Context Protocol server for Claude and other AI assistants, off until you turn it on. See [below](#ai-assistants-mcp). |

![The Performance page's CPU view: ten minutes of total and kernel time with two earlier builds and the one running now, the busiest processes, and one graph per logical processor naming the process that ran on it](docs/screenshots/performance.png)

<sub>The screenshots show a made-up PC, not anyone's real one.</sub>

And across the pages:

- **Not responding and stuck processes**: hung windows are marked; wait
  chains show what each thread waits for, deadlocks first; a process that
  cannot finish ending says what holds it (handles still open to it, a
  driver that strips the right to end it, Windows' protection).
- **Leaks**: handle, memory and thread growth that keeps going is flagged.
- **Memory dumps**, small or full, taken from a snapshot so the process
  pauses only for a moment.
- **Game mode** (optional): while a game is in front, busy background
  programs run at below normal priority until it ends. The game, Windows'
  core processes, audio and anti-cheat software are never touched.
- **Notification area** (optional): closing the window leaves Anabolic
  running, recording the history and applying rules; alerts become
  notifications.
- **Several monitors**: called up during a full-screen game, the window opens
  on another monitor (Windows: on Linux the compositor places windows).

## On Linux

The same pages, read from Linux's own accounting: `/proc` and `/sys`, the
kernel's perf events and netlink interfaces, fanotify, and systemd and the
desktop over D-Bus. No kernel module, no eBPF program. Where Linux works
differently, the pages say how:

| Page | On Linux |
|---|---|
| **Processes** | The kernel's threads (kworkers, kswapd, the interrupt threads) are counted under one *Kernel* row, PID 2, as Windows counts its own under System; its details list them one by one. Handles are open files. A program's package stands where Windows shows its company, and its file is checked against the package's record. Dumps are core files that gdb reads. |
| **Performance** | Who ran on each core, interrupt load and the processes behind each disk come from perf events. Intel and AMD GPUs through the kernel's DRM accounting; NVIDIA through NVML, asked only while the GPU is awake, so a laptop's sleeping one stays asleep. Every temperature, fan and power sensor the kernel's hardware monitors know, per core included; a sensor on a sleeping device is left out rather than woken. Partitions, with what LUKS and LVM hold, from udev. Monitors from EDID and the desktop's own layout (KDE, GNOME). |
| **Services** | systemd's: the system's and your user's. Start, stop, restart, and enabled, disabled or masked; there is no pausing. |
| **Startup** | Autostart entries and systemd user units, turned off the way the desktop does it: a system-wide entry is hidden by a copy in your own folder. Nothing is deleted. |
| **Stutters** | Interrupt and softirq time per handler, in the manner of LatencyMon. Linux reports no frame times to anyone outside a game, so frame times and 1 % lows are Windows only. |
| **Network** | TCP traffic per program and per connection; Linux counts no UDP traffic per program. Its firewalls filter by address, port and user, not by program, so blocking a program is Windows only, as is the list of DNS lookups. |
| **Activity** | Every file, program and connection a process touches. There is no registry; instead, a program or process can be recorded with its system calls, as strace shows them, with a count, the failures and the time spent per call. They come from the kernel's tracepoints: the process is never stopped or read. |

Around them: the notification area icon is a StatusNotifierItem (KDE has
it; GNOME needs the AppIndicator extension), the hotkey goes through the
desktop's GlobalShortcuts portal, and game mode finds the window in front on
KDE and on X11. The keyrings (KWallet, GNOME Keyring, ssh-agent, gpg-agent)
take lsass' place as what is not dumped unless allowed.

## Getting started

Download the zip for your system from the
[releases](https://github.com/RaptorTwentyOne/Anabolic/releases) page and
unpack it anywhere: a single executable with nothing to install.

### Windows

Windows 10 or 11, x64. Run `anabolic.exe`.

On an administrator account Windows asks for elevation as it starts, and with
it Anabolic sees everything: kernel traces, every process' details, disk
counters. On a standard account it runs without, shows what it can and says
what is missing and why; Settings has **Restart as admin**.

> [!NOTE]
> Releases are not code-signed yet, so Windows SmartScreen may warn on first
> run. Choose **More info** → **Run anyway**, or build it yourself.

### Linux

x64, a C library from 2022 or later (glibc 2.35: any current distribution),
and a Wayland or X11 desktop with OpenGL.

```
./anabolic --install   # once, optional: the application menu and the taskbar icon
./anabolic
```

Anabolic runs as you. Without more it sees your own processes in full and
others' in part. **Grant full access** (top right, or in Settings) gives the
executable the capabilities it needs, once: pkexec asks for your password and
runs `setcap` on it. The window stays yours, on your desktop session; only
those rights come with it. As anyone who can run the file has them too, it
is made yours alone (mode 700) first. Kernel traces, other users' processes'
files and threads, ending them, and closing connections need it.

### Updates

Anabolic keeps itself up to date. Soon after it starts, and then once a day,
it asks GitHub whether a newer release is out. A new release is downloaded,
checked against the SHA-256 checksum GitHub lists for it, and put in place of
the old executable; it runs from the next start, or at once from the bar that
says it is installed. In Settings → Updates it can ask first instead, or not
check at all. Copies built from source are left to their source. On Linux the
download goes through curl, and the new file has to be granted full access
again: the rights belong to the file they were given to.

### Bringing it up from anywhere

**Ctrl+Alt+Esc** brings Anabolic to the front from any program while it runs;
Settings offers other keys, or none. With *Keep running in the notification
area when closed* on, closing the window leaves Anabolic running, so the key
works all day. Windows' Task Manager and Ctrl+Shift+Esc are left as they are.

On Linux the desktop asks once whether Anabolic may have the key. KDE gives
Ctrl+Alt+Esc to *Kill a window*: choose another key in Settings, or free that
one in KDE's shortcut settings.

### Command line

| Option | |
|---|---|
| `--tray` | Start in the notification area, without a window. |
| `--new` | Start another instance instead of bringing the running one to the front. |
| `--install`, `--uninstall` | Linux: add Anabolic to the application menu (a .desktop file and the icon in `~/.local/share`), or take it away. |

## Keyboard

| Keys | |
|---|---|
| Ctrl+1 … Ctrl+9, Ctrl+0 | Pages, in the order of the rail; Ctrl+0 is MCP |
| Ctrl+, | Settings |
| Ctrl+F | Filter processes |
| Ctrl+T | Tree, list or apps |
| Ctrl+H | Find handle: who has this file or object open |
| Space | Pause updates |
| F5 | Refresh now |
| Del, Shift+Del | End the selected process, or its whole tree |
| Shift+F10 | The selected process' menu |
| F1 | Every shortcut and what the colours mean |

## AI assistants (MCP)

The MCP page (Ctrl+0) turns on a Model Context Protocol server, so Claude and
other MCP clients can read what Anabolic sees and do what you allow. It speaks
Streamable HTTP at `http://127.0.0.1:47123/mcp` (the port can be changed),
listens on this computer only, and every request needs the access key the
page shows. The page gives the command for Claude Code and the JSON for other
clients, and lists the clients that connected and every call they made.

What clients may do is chosen on the page, kind by kind; a call of a kind
that is off is refused and says where to turn it on:

| Kind | Tools |
|---|---|
| Read what the PC is doing (on by default) | overview, CPU, memory, disks and their partitions, GPUs, temperatures and fans, monitors, hardware, stutters, history, processes and their tree, one process in full (who started it, what it hosts, its connections), why a process will not end, leaks, connections, open ports, network usage, DNS lookups, services, startup programs |
| Turn kernel traces on for a while | who runs on each core, which files each disk serves, traffic per connection, frame times |
| Look inside a process | handles, modules, thread stacks, wait chains |
| Write memory dumps | small or full dumps (never of lsass, or on Linux of the keyrings) |
| Record what processes do | Process Monitor style recordings, of a process, a name or the whole machine; on Linux, a process' system calls too |
| End processes | end a process or a tree |
| Suspend, resume and reprioritise | suspend, resume, priority, cores, efficiency mode |
| Control services | start, stop, restart, pause, startup type |
| Close connections, block programs | close a TCP connection; Windows Firewall block and unblock (Windows) |
| Turn startup programs on and off | the way Task Manager does |

Tools that open a process (inspection and dumps), and actions on a game or an
anti-cheat program, refuse the first call with the risks in words: the
assistant has to tell you and call again with `force=true` only if you agree.
The system's critical processes (Windows' own; on Linux init, the kernel and the
session's compositor) and Anabolic itself are never acted on.

The server also offers resources (`anabolic://overview`,
`anabolic://process/{pid}` and others) and prompts for common questions
("why is the PC slow", "why does my game stutter", "what happened at 14:32").

## Games and anti-cheat

Anabolic watches from system-wide tables and kernel traces: its regular views
do not open game processes, read their memory or inject anything. What does
open a process (memory dumps, a thread's call stack, duplicating handles,
reading modules) asks first. Game mode never touches the game, Windows' core
processes or anti-cheat software.

The process that holds Windows' sign-in secrets (lsass) is not dumped unless
*Allow dumps of lsass.exe* is turned on in Settings: its memory is what
credential-stealing tools take, so antivirus software stops any program that
reads it and may quarantine Anabolic. MCP clients can never dump it.

On Linux the keyrings (KWallet, GNOME Keyring, ssh-agent, gpg-agent) hold the
session's passwords and keys unlocked, and are not dumped unless *Allow dumps
of the keyrings* is on.

How an anti-cheat reacts to any program is up to its vendor; Anabolic makes
no promise on their behalf.

## Privacy

Anabolic has no telemetry. The one request it makes by itself is the update
check: GitHub's API is asked for the latest release, and the request carries
nothing about this PC beyond what any download does (its address, and
`Anabolic/<version>` as the program asking). Settings → Updates turns it off.

Looking a file up on VirusTotal or an address on ipinfo.io opens your
browser, and only when you ask. Remote addresses get their names from the
system's DNS resolver.

Everything it keeps stays on this computer:

- `%APPDATA%\Anabolic` on Windows: settings.
- `%LOCALAPPDATA%\Anabolic` on Windows, `~/.local/share/anabolic` on Linux
  (settings there too): the history (process names and command lines, which
  can include whatever a program was started with), memory dumps and exports
  you ask for, and `stalls.log`, the moments Anabolic's own window fell
  behind. Turn the history off or shorten it in Settings.

## Building from source

Rust 1.95 or newer; on Windows with the MSVC toolchain. On Linux no
development packages are needed: Wayland, X11, OpenGL and the rest are
loaded as the program starts.

```
cargo build --release
```

The result is `target\release\anabolic.exe` or `target/release/anabolic`, a
single executable with its fonts and icons embedded. `cargo test` runs the
tests; the few that need administrator rights (on Linux, full access) skip
themselves without them.

The workspace has two crates: `crates/anabolic-core` reads and controls the
system (process scans, kernel traces, handles, services, the history) and has
no interface; `crates/anabolic` is the egui application and the MCP server.
Each core module says what it means on every system; what reads it lives
beside it, in `windows/` and `linux/`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.

The Aldrich font is under the SIL Open Font License
([crates/anabolic/assets/fonts/OFL.txt](crates/anabolic/assets/fonts/OFL.txt)).
