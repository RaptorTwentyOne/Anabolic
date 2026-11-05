# Changelog

What changed in each release of Anabolic, newest first. Anabolic carries
this file inside it: the first time a new version starts, it shows what is
new since the one before.

## Unreleased

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
- **Boots** (Startup page): how long each of the last boots took, the
  firmware's part and the system's to the sign-in, what kind of start it
  was, and how the shutdown before it went: clean, a blue screen with its
  stop code, the power button held, or none at all. Where Windows still
  keeps its own boot measurement, what it found slower than usual. From
  the system's own logs (systemd on Linux). `anabolic-cli boots`, and
  `get_boot_history` for MCP clients.
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
