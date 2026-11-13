# Changelog

What changed in each release of Anabolic, newest first. Anabolic carries
this file inside it: the first time a new version starts, it shows what is
new since the one before.

## Unreleased

- **Data usage** (Network page), in the manner of NetworkUsageView: what
  each program received and sent over the last day, week, month or two,
  from Windows' own record (the one Settings' Data usage page reads),
  from before Anabolic was installed too. `anabolic-cli usage`, and
  `get_data_usage` for MCP clients.
- **Upload limits** (Rules page): a rule can hold what a program sends
  to a rate, through Windows' QoS policy, the part of NetLimiter Windows
  can do without a driver of its own; downloads cannot be held that way.
  The limits last until Anabolic quits or Windows restarts.
- **Wi-Fi** (Network page), in the manner of WifiInfoView and inSSIDer:
  the network this PC is on with its channel, signal and link speed,
  how crowded each channel is, the least crowded one to move the router
  to, and every access point heard with its band, signal, security and
  Wi-Fi generation. `anabolic-cli wifi`, and `get_wifi` for MCP clients.
- **Driver packages** (Performance > Devices), in the manner of
  DriverStore Explorer: every third-party driver package Windows keeps,
  with its maker, kind, version, date, size and the devices that use
  it; the old versions no device uses (graphics drivers leave one per
  install) are removed after a question, and Windows refuses to remove
  one a device uses.
- **What changed** (Crashes page), in the manner of Reliability Monitor
  and WinUpdatesView: updates installed and those that failed, drivers
  installed or updated, programs installed and removed, services and
  kernel drivers set up, day by day beside each day's crashes. On Linux,
  the package manager's log. `anabolic-cli changes`, and `get_changes`
  for MCP clients.
- **PC report** (Overview, This PC), in the manner of Speccy: the
  system and firmware, processor, memory and its speed, graphics with
  their drivers, monitors, drives with their health and free space,
  network adapters, what the checkup found, devices with a problem and
  the week's crashes, as Markdown on the clipboard for a forum post or
  a support ticket. No names, serial numbers or addresses.
  `anabolic-cli report`, and `get_pc_report` for MCP clients.
- **Explorer's right-click menu** (Settings, off until turned on), in
  the manner of LockHunter: "Who is using this?" on files and folders
  opens Find handle on the path, and "What fills this folder?" on
  folders and drives opens Disk space there. With Anabolic already
  running, the request goes to it. `anabolic --locks PATH` and
  `anabolic --space PATH` do the same from a shortcut or a script.
- **VirusTotal**, in the manner of Process Explorer: with your own API
  key (Settings, kept encrypted for your account), a module, a file or
  a startup entry is checked in place by its hash: how many engines flag
  it and what most call it; the Startup page's Everything checks every
  entry that is not Windows' own, one every 15 seconds as a public key
  allows. Only hashes are sent. Without a key, the VirusTotal buttons
  open the file's page in the browser, now on startup entries too.
- **Disk speed test** (Performance, each disk), in the manner of
  CrystalDiskMark: large files and small pieces, one request at a time
  and many at once, read and written past the system's cache on a test
  file of 256 MB to 4 GB, which is deleted when the test ends (Windows
  deletes it even if Anabolic does not get to).
- **Hosts and DNS** (Network page): which DNS server answers fastest
  from here, the one in use against Cloudflare, Google, Quad9 and the
  rest, timed in the manner of GRC's DNS Benchmark (from its cache, and
  looked up); what the DNS cache holds, emptied at a click; and the hosts
  file, edited in place and saved after a question, the old one kept
  beside it. `anabolic-cli dns`, `anabolic-cli hosts`, and
  `benchmark_dns`, `get_hosts_file` for MCP clients.
- **USB history** (Performance > Devices), in the manner of USBDeview:
  every USB device this PC has known, the last connected first, with
  when each was first and last connected and last removed, connected
  now or not. `anabolic-cli devices --usb`.
- **Connection quality** (Network page, Quality), in the manner of
  PingPlotter and WinMTR: a ping a second to the router, the DNS server,
  the internet and the server a program talks to most (a game in front
  first), each with its delay, jitter, lost replies and the last two
  minutes as a strip, and in words where the trouble lies: between the
  PC and the router (the Wi-Fi or the cable), past it (the line or the
  provider), or only on the way to the server. A route traced hop by hop
  shows where delay and loss begin. Only ICMP echo requests are sent.
  `anabolic-cli ping [ADDRESS] [--route]`, and `check_connection` for
  MCP clients.
- **For gamers**, what three small tools did:
  - *Timer resolution* (Stutters page): how often Windows' clock
    interrupt fires now, and every program that asked for a finer one
    and what it asked for, as TimerResolution and `powercfg /energy`
    show it. `anabolic-cli timer`, `get_timer_resolution`.
  - *Interrupts* (Performance > Devices): how each device interrupts the
    processor, by message (MSI, MSI-X) or the shared wire, what its
    hardware can do and what it will do after a restart, in the manner
    of the MSI utilities; a device can be switched to MSI or back, and
    its interrupts held to one processor, after a question. Drive
    controllers are only shown: if their driver could not start the new
    way, Windows could not start. `anabolic-cli interrupts`,
    `list_interrupts`.
  - *Standby list cleaner* (Rules page), in the manner of ISLC: the
    standby list cleared by itself when it holds at least a set amount
    and free memory falls under another, only while a game is in front
    if asked, at most once a minute.
- **Checkup** (Performance page): what costs speed without saying so,
  the things otherwise looked up one by one in CPU-Z, GPU-Z, HWiNFO and
  Windows' settings. Memory below the speed its part number is sold at
  (EXPO or XMP off), every module on one channel, mixed kits, no page
  file; a graphics card or NVMe drive on fewer PCIe lanes or an older
  generation than it has; Resizable BAR; Xbox Game Bar's background
  recording, Game Mode, hardware-accelerated GPU scheduling; a monitor
  below its refresh rate or resolution; the power plan and power mode,
  USB selective suspend, fast startup, a restart updates wait for; TRIM;
  a gigabit port held to 100 Mbps by its cable; memory integrity; mouse
  acceleration. Each says what it is now, what it costs and where to
  change it; nothing is changed for you. The Overview names what to
  change. On Linux, the PCIe links, swap, the processor's frequency
  policy, a restart the packages wait for and TRIM. `anabolic-cli
  checkup`, and `get_checkup` for MCP clients.
- **Crashes** (Ctrl+E): what crashed, from Windows' own records. By
  program, with how many times and the module most of its crashes were in,
  or each one: the exception in words ("it used memory that was not its
  own"), the module and where in it, how long the process had run, a .NET
  program's exception with its stack, and the dump Windows kept. Also the
  programs that hung and were closed or stopped responding a while, the
  ones Windows' resource exhaustion detector thought were leaking, display
  drivers it had to reset, and stop errors. Error Reporting's report
  folders reach back past the event logs. The Overview names a program
  that went away three times in a day. From systemd-coredump on Linux.
  `anabolic-cli crashes`, and `get_crashes` for MCP clients.
- **Everything that starts** (Startup page): every place things start
  from by themselves, in the manner of Autoruns: Run keys, Startup
  folders and Active Setup, Explorer's extensions, Winlogon, services
  and drivers set to start, every scheduled task, debugger hijacks and
  the command prompt's AutoRun, AppInit and Known DLLs, BootExecute, LSA
  packages, print monitors and network providers, each with the file it
  runs and who signed it. Windows' own are hidden unless asked; what
  is not signed, not there or off Windows' default comes first. On Linux,
  systemd units and timers, cron, autostart entries, the preload list,
  shell start files and modules loaded at boot. `anabolic-cli autoruns`,
  and `list_autoruns` for MCP clients.
- **Devices** (Performance page): every device as Device Manager knows
  it, the ones with a problem first and the problem in words ("Windows
  stopped it: it reported a problem", Code 43) with what usually helps,
  the driver each runs on (who made it, its version and date, the file it
  was installed from), and the devices known but no longer connected,
  with when each was last there. The Overview names a device with a
  problem. On Linux, the PCI and USB devices and their kernel drivers.
  `anabolic-cli devices`, and `get_devices` for MCP clients.
- **Memory** (a process' details): its memory region by region, in the
  manner of VMMap: its program and DLLs, mapped files, heaps, thread
  stacks and private data, each with what is committed and what is in RAM
  (and of that what only it uses), largest first, with what each region
  is. Read it again later and it shows what grew. Private memory that may
  be executed is marked. It opens the process, after asking.
  `anabolic-cli memmap PID`, and `get_memory_map` for MCP clients.
- **History** (a process' details): its program over the last 24 hours
  or 7 days, whatever its processes and PIDs were: CPU, memory, disk and
  network minute by minute, and each day's processor time, most memory
  and bytes moved. The history now records every program's minutes (the
  ones that did something, and every ten minutes the ones holding memory),
  a few MB a day more. `anabolic-cli program NAME`, and
  `get_program_history` for MCP clients.
- **Give memory back** (Performance, Memory): empty the standby list, only
  its low-priority part, the modified list, every working set or the file
  cache, as RAMMap does, each with what it does and when it helps. On
  Linux, the page cache.
- **Live kernel dump** (Stutters page, and the System process' menu): the
  running kernel's memory for WinDbg, written without stopping anything,
  as Task Manager writes one: what a driver is doing while the PC stutters
  or hangs.
- **Anabolic's own memory stays put.** It kept a little of every process
  it saw start (its command line, its family, its program's icon), and a
  build starts thousands: some 10 KB each, over 100 MB an hour on a
  developer's PC, gigabytes over days. Command lines now go ten minutes
  after their process, family records past 20,000 and icons past 3,000
  programs, paused view or not. A Disk space scan out of sight for 20
  minutes is let go (a drive's tree holds hundreds of megabytes), and the
  page offers to scan it again; the drive's changes are followed only
  while the page is on screen, and picked up where they stopped. Notices
  no longer pile up while the window is hidden.
- **On crates.io.** `cargo install anabolic` builds and installs Anabolic
  and anabolic-cli; on Windows `cargo binstall anabolic` takes the release
  build instead. A copy installed with cargo says so under Settings →
  Updates and is updated with cargo, not replaced by a release.

## 0.7.1 (2026-11-06)

Anabolic starts on a Windows that has never had the Visual C++ runtime.

- **Starts on a fresh Windows.** The exes carry the Visual C++ runtime
  inside them. Before, they needed the Visual C++ Redistributable, which
  most PCs have from some other program but a fresh Windows does not;
  without it Windows said VCRUNTIME140.dll was not found and Anabolic did
  not start.

## 0.7.0 (2026-11-05)

What fills the drives, in the manner of WinDirStat and WizTree and never
slower for it; how the drives themselves are holding up; each boot step by
step, with how the session before it ended; and where a process spends its
processor time.

- **Disk space** (Ctrl+D): what fills a drive or a folder, in the manner
  of WinDirStat and WizTree: the folders as a tree, the file types and the
  largest files, above a treemap of the whole. On Windows an NTFS drive is
  read from its master file table when Anabolic runs as administrator, a
  drive of seven million files in seconds; otherwise folders are read on
  several threads at once. The map is laid out and drawn on a thread of
  its own, so the window never waits for it. What is no longer needed goes
  to the Recycle Bin (the trash on Linux) or is deleted for good, after a
  question, and the sizes follow without scanning again; Windows' own
  files and folders, and the system's on Linux, are refused.
- **Clean up**: what the scan found that can go, said in words with how
  safe deleting it is: temporary files, browser, shader and package caches,
  build output marked as a cache, projects' node_modules, older JetBrains
  versions, crash dumps, the Recycle Bin. What Windows cleans with its own
  tools (update downloads, Windows.old, the hibernation file, WSL and
  Docker disks) comes with the tool to use instead.
- **Duplicates**: files with the same contents, compared by their start,
  middle and end and then whole, read in the background at the lowest
  priority for the processor, the disk and memory. Files kept in the cloud
  and a file's second names are left out.
- **Changes since**: each scan leaves an outline of its larger folders, and
  a later scan of the same folder shows what grew and shrank since.
- **Live**: after a scan from the master file table, NTFS's change journal
  is followed, so the sizes stay true as files are made, written and
  deleted; the Live tab lists what changed since the scan, and the program
  that wrote it when the disk trace sees it.
- The folder picker chooses a folder to scan; the lists save as CSV; the
  line between the lists and the map moves, and stays where it was put.
  The arrows move through the lists as in Explorer: Left and Right close
  and open the folders, Enter too.
- Every text field has a button at its end that empties it, the process
  filter's among them.
- The Overview shows how full each drive is; a click on one opens Disk
  space with what fills it.
- **Profile** (a process' details): where it spends its processor time.
  For a few seconds the system's own profiler samples what each
  processor runs, with the call stack, a thousand times a second; the
  samples are summed by function (its own time and with what it calls),
  as a call tree, by module and by thread. The process is never opened:
  the kernel walks its stacks, and names come from the symbols on this PC
  or the modules' exports. Windows, as administrator. `anabolic-cli
  profile PID`, and `profile_process` for MCP clients.
- **Drive health**: each disk on the Performance page shows how it is
  holding up, as the drive reports it: an NVMe drive's health log, a SATA
  drive's SMART attributes. Wear used, temperature, hours, power cycles,
  unsafe shutdowns, what it wrote, spare blocks, and the sectors and
  errors that mean it is failing, with a verdict in words (Good, Watch it,
  Failing) and the attributes in full. Read in the background every half
  hour; a sleeping drive is not woken. The Overview warns of a failing
  drive, and `get_disks` gives MCP clients the same. On Linux through
  smartmontools.
- **Boots** (Startup page): each of the last boots in its steps (the
  firmware, the loader, Windows to the sign-in screen, the sign-in to the
  desktop; the time the sign-in screen waited for someone apart), with
  Group Policy's time and the slow sign-in steps. How the session before
  ended: who asked for the restart or shutdown and why (Windows Update, an
  installer, you from the Start menu), how long shutting down took and
  how long the PC was off; or that it never shut down, with when it was
  last seen running, a blue screen's stop code and dump. What failed as it
  started (services that did not start or answer, drivers that did not
  load), and how the session went: how long it ran, its sleeps, the
  services that stopped unexpectedly. From the system's own logs (systemd
  on Linux). `anabolic-cli boots`, and `get_boot_history` for MCP
  clients.
- Shift+Delete on the Processes page ends the selected process' tree on
  Windows too: Windows hands the keys on as Cut, and they did nothing.
- `anabolic-cli space [PATH]` shows the same in a terminal: the folders as
  a tree, or the largest files (`--files`) or file types (`--types`). MCP
  clients get it as `get_disk_usage`, which keeps a scan for ten minutes so
  questions about the folders inside it are answered at once.

## 0.6.0 (2026-11-04)

Anabolic in a terminal, its MCP server for an assistant to start without
the window, and services made, changed in full and deleted from the
Services page.

- **Terminal commands:** `anabolic-cli ps`, `kill`, `services`, `service
  restart NAME`, `ports` and more (on Linux `anabolic ps`): what the
  window shows, as tables, or as JSON for scripts (`--json`). Each answers
  through the MCP tool behind it, and refuses what that refuses. On
  Windows they come with anabolic-cli.exe, beside anabolic.exe, and the
  installer puts its folder on PATH.
- **MCP without the window:** a client can start Anabolic itself
  (`anabolic-cli mcp`) and speak to it over standard input and output: no
  port, no key, no window. The MCP page shows the command and the JSON
  for it. It may do what the page allows, except setting limits.
- **Services: new, properties, delete.** Create a service, change
  everything about one (program, account and password, dependencies, what
  it does when it fails), or delete one; Windows' own services can be
  disabled, not deleted. On Linux a new unit goes where systemd keeps the
  administrator's own, and a package's unit is changed through a drop-in.
- Each release file carries a build attestation, which `gh attestation
  verify` checks.

## 0.5.0 (2026-11-03)

Limits Windows keeps on a program's processor and memory, rules that wait
for a program to be busy or in the background and act when something goes
wrong, and Anabolic in the notification area again from the moment you
sign in.

- **CPU and memory limits** (a process' menu, and rules): a process, with
  what it starts, held to a share of the whole machine's processor and to
  an amount of private memory. Windows keeps the cap however busy the
  machine is and whatever the priority. Past the memory limit a program's
  requests for more fail, and most then stop with an error. A rule's limit
  holds all of the program's processes together. Limits last while
  Anabolic runs, and leave Windows' core processes and anti-cheat programs
  alone. On Linux, where systemd holds such limits per service, Anabolic
  says how to set them there.
- **Rules that wait:** a rule's priority, efficiency mode and CPU limit can
  apply only while the program is busy (over a share of the CPU for a
  while) or in the background, and go back when that passes: once it has
  calmed down, or as one of its windows comes to the front. A priority you
  changed meanwhile stands.
- **Rules that act:** each of a rule's watches (private memory, CPU,
  handles, and now a window that stops responding for a while) can write a
  small or full dump, end the process, or write the dump and then end it,
  besides telling you. The dump comes first, so it shows the process as it
  was. A rule writes at most one dump every ten minutes, and Windows' core
  processes and anti-cheat programs are only told about.
- **A Status column** in the process list, beside the name as in Task
  Manager: efficiency mode, suspended, not responding, and limited. It
  joins the columns shown, and the column menu takes it away. A process'
  menu now offers to leave efficiency mode by what Windows says of it,
  rather than by its priority.
- **Game mode without a game:** lowering busy background programs can go
  on all the time, sparing the program in front.
- **Quitting puts things back:** what a rule's condition or game mode
  changed (a priority, efficiency mode) goes back as Anabolic quits, as
  limits are lifted.
- **Starting as you sign in** puts Anabolic in the notification area again,
  without its window. Windows can start it before the taskbar is there:
  the icon used to give up and show the window, and closing that window
  quit. Now the icon waits for the taskbar, and the window shows itself
  only when no taskbar comes within a minute.
- **The icon comes back** when Explorer restarts, for an Anabolic running
  as administrator too.
- **Closed into the notification area,** Anabolic no longer draws its
  window for nobody, and the icon tells of rule and memory alerts, a
  program in many copies and an installed update again: they had gone to
  the hidden window instead.

## 0.4.1 (2026-10-26)

An installed Anabolic that updated itself can be installed again, or
repaired, from the release's installer.

- **Updates through the installer** keep the installer's name from the
  release. Windows remembers the file a program was installed from, and
  installing the same version again (as winget does) looked for a file of
  that name and failed. This holds from the next update on: 0.4.0 installs
  this one the old way.

## 0.4.0 (2026-10-26)

A page that says how the machine is doing, quicker ways to act on what it
shows, and tables whose columns go where you drag them.

- **Overview** (Alt+Home, first on the left): a tile for CPU, memory, disk,
  network and GPU, each with its graph and the app behind most of it; what
  needs attention, with what can be done about it; the busiest apps;
  development ports; what started in the last ten minutes; and this PC in a
  few lines. A click on an app shows its processes alone in the list; a
  right click has what can be done with it.
- **One program in many copies:** when a program runs 50 times over from the
  same starter, usually a script or a build gone wrong, a bar says who
  started them and ends them all at a click; with the window closed, a
  notification does. Browsers and Windows' service hosts run many processes
  by design and are left out. Settings → Warnings changes the number or
  turns it off.
- **Quick filters** above the process list: CPU, memory, disk, network, GPU,
  started in the last ten minutes, services. Several at once narrow the list
  together, and with the text filter.
- **The details have a close button,** as Esc does.
- **Columns in every table:** drag a title to move its column. The order
  and the widths stay, after a restart too; a right click on the titles
  has Reset columns. The process list's column menu now stays open while
  columns are turned on and off, and turning one on or off no longer resets
  the others' widths.
- **Tables scroll sideways** when their columns are wider than the window
  (Shift and the wheel, or the bar at the bottom), instead of squeezing the
  last column to nothing.
- **A row's menu stays with its row:** a list that sorts itself again as
  its numbers change (by CPU, say) no longer moves an open menu onto
  another process.
- **Run new task** (Ctrl+N, or on the Overview): a program, folder, document
  or web address, with administrator rights or without. Without them it
  runs as you do, even from an elevated Anabolic. On Linux it runs as you;
  a program that needs root asks for it itself.
- **Notify me when it exits** (a process' menu): a notification when it
  ends, with how long it ran and its exit code.
- **Development ports** (a tab of the Network page): the dev servers,
  databases and debuggers listening, named by their tool (Vite, PostgreSQL,
  the Node.js debugger and more), and the ports you watch, free or with who
  holds them. Watched ports can carry a name of your own, here or in
  Settings. Free port… ends what holds one, after asking; when a service or
  Windows itself holds it, Anabolic says so instead.
- **CPU on the notification area icon:** a bar up its right edge, amber
  from 90 %. Settings → Window turns it off.

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
