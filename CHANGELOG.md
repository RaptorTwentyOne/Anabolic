# Changelog

What changed in each release of Anabolic, newest first. Anabolic carries
this file inside it: the first time a new version starts, it shows what is
new since the one before.

## 0.3.0 (2026-10-23)

Anabolic now has an installer, can start as you sign in, and says what is
new when it updates.

- **An installer for Windows:** `anabolic-…-windows-x64.msi` puts Anabolic
  in Program Files and the Start menu, and Settings → Apps removes it again.
  The zip stays, for those who want a single file; a copy from the zip keeps
  updating from the zip. To move to the installer, run it, then delete the
  zip's folder: settings and history stay where they are.
- **Installed copies update through the installer,** without a window, so
  Settings → Apps always shows the version that runs.
- **Start when I sign in** (Settings → Window): Anabolic starts in the
  notification area as you sign in. On an administrator's account a task in
  Task Scheduler starts it elevated, without asking; that takes the
  installed copy, since a file started elevated has to be where only
  administrators can change it.
- **What's new:** the first time a new version starts, a small window says
  what changed since the version before.
- **Signing out and shutting down:** Anabolic saves its settings and stops
  its kernel traces before Windows ends it, instead of losing what changed
  since the last save.

## 0.2.1 (2026-10-20)

Updates that fail for a moment no longer wait a day.

- **Updates:** started straight from a zip just unpacked, Anabolic is read
  by SmartScreen and the virus scanner for a few seconds, and meanwhile
  Windows does not let the update move it aside. Now the update waits up to
  ten seconds for such a file, and an install that still fails is tried
  again after a minute, five and thirty.
- The bar says when it tries again before why it failed, so a long path in
  the reason no longer hides the rest.

## 0.2.0 (2026-10-20)

Anabolic now runs on Linux too, and on both systems it shows each disk's
partitions, every sensor it can read, and goes back and forward like a
browser.

- **Linux:** the same pages, read from `/proc` and `/sys`, the kernel's perf
  events and netlink, fanotify, and systemd and the desktop over D-Bus. No
  kernel module, no eBPF program. Where Linux works differently, the page
  says how.
- **Partitions:** each disk's partitions in order, with their type, file
  system, label, mount point, space, and the space left unallocated; on
  Linux, with what LUKS and LVM hold.
- **Sensors:** every temperature, fan and power reading, grouped by device,
  with the hardware's own limits. A sleeping device is not woken to be read.
- **Back and forward:** Alt+← and Alt+→, or the mouse's back and forward
  buttons, through the pages and processes visited.
- **Activity:** New recording keeps the last recording, and Back returns to
  it.

## 0.1.1 (2026-10-18)

Anabolic now keeps itself up to date.

- **Updates:** soon after it starts, and then once a day, Anabolic asks
  GitHub whether a newer release is out. A new release is checked against
  the SHA-256 checksum GitHub lists for it, and runs from the next start, or
  at once from the bar that says it is installed.
- Settings → Updates can make it ask first, or not check at all. The check
  carries nothing about your PC.

## 0.1.0 (2026-10-17)

The first release: a Windows task manager that stays responsive when the
machine does not, and answers *who is doing this, and why*, not only *how
much*.

- Processes as a tree, list or apps; who ran on each CPU core and which
  files each disk serves.
- Stutters with frame times, and DPC/ISR time per driver.
- Traffic per program, connections and DNS lookups.
- Process Monitor style activity recording, and a 7-day history.
- Services, startup programs, per-program rules and game mode.
- An optional MCP server for Claude and other AI assistants.
