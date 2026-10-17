# Anabolic

A Windows task manager that stays responsive when the machine does not, and
that answers "who is doing this, and why" rather than only "how much".

Windows 10 and 11, x64.

## What it does

- **Processes** as a tree, a flat list or grouped by app, with CPU, memory,
  disk, network, GPU, handles and command lines. End, suspend, priority,
  affinity, efficiency mode. Details show threads with their call stacks,
  modules with their signatures, handles, network connections, the services
  a process hosts and the chain of processes that started it.
- **Performance**: who ran on each CPU core, which process reads and writes
  which file on each disk, memory, network adapters, GPUs and monitors.
- **Not responding and stuck processes**: hung windows, wait chains with
  deadlocks first, processes that cannot finish ending and what holds them.
- **Stutters**: frame rate, 1 % lows and frame times of every program that
  draws, each stutter with what else happened at that moment, and DPC/ISR
  time per driver.
- **Network**: traffic per program, every connection, open ports, DNS
  lookups and who made them; closing a connection, blocking a program in
  Windows Firewall.
- **Activity**, Process Monitor style: every file, registry key,
  connection and program a process touches, step by step, recorded from a
  kernel trace. Record one program, a process tree or the whole computer and
  filter afterwards.
- **History**: every 10 seconds, the machine's totals and the processes that
  led, kept for 7 days by default, to answer "what happened at 14:32".
- **Services**, **Startup** programs, per-program **Rules** (priority,
  cores, alerts) and an optional **Game mode** that calms background
  programs while a game is in front.
- **Leaks**: handle, memory and thread growth that keeps going is flagged.

It stays usable under load: it raises its own priority, never blocks its
interface thread and takes about 1 ms of CPU per frame.

## Running it

Run `anabolic.exe`. On an administrator account Windows asks for elevation
as it starts, and with it Anabolic sees everything (kernel traces, every
process' details). On a standard account it runs without, shows what it can
and says what is missing and why.

Command line:

| Option | |
|---|---|
| `--tray` | Start in the notification area, without a window. |
| `--new` | Start another instance instead of bringing the running one to the front. |

### Bringing it up from anywhere

Ctrl+Alt+Esc brings Anabolic to the front from any program while it runs;
Settings offers other keys, or none. With "Keep running in the notification
area when closed" on, closing the window leaves Anabolic running (still
recording the history and applying rules), so the keys work all day. During
a full-screen game the window opens on another monitor. Windows' Task
Manager and Ctrl+Shift+Esc are left as they are.

## Games and anti-cheat

Anabolic watches from system-wide tables and kernel traces: its regular
views do not open game processes, read their memory or inject anything.
What does open a process (memory dumps, a thread's call stack, duplicating
handles, reading modules) asks first. Game mode never touches the game,
Windows' core processes or anti-cheat software, and dumps are never written
for the process that holds Windows' sign-in secrets (lsass).

How an anti-cheat reacts to any program is up to its vendor; Anabolic makes
no promise on their behalf.

## Privacy

Anabolic sends nothing anywhere by itself and has no telemetry. Looking a
file up on VirusTotal or an address on ipinfo.io opens your browser, and
only when you ask. Remote addresses get their names from Windows' DNS.

Everything it keeps stays on this computer:

- `%APPDATA%\Anabolic`: settings.
- `%LOCALAPPDATA%\Anabolic`: the history (process names and command lines,
  which can include whatever a program was started with), memory dumps
  and exports you ask for, and `stalls.log`, the moments Anabolic's own
  window fell behind. Turn the history off or shorten it in Settings.

## Building

Rust 1.95 or newer with the MSVC toolchain:

```
cargo build --release
```

`target\release\anabolic.exe` carries everything it needs, including the
launcher, which `crates/anabolic/build.rs` builds. `cargo test` runs the
tests; the few that need administrator rights skip themselves without them.

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
