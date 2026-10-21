# Privacy policy

Anabolic runs on your computer and has no account, no telemetry and no
server of its own. This page says what it sends anywhere, what it keeps,
and what its MCP server hands to the programs you connect to it.

Last updated: 2026-10-21.

## What leaves the computer

- **The update check.** Once a day (and when you press *Check now*)
  Anabolic asks GitHub's API for the latest release of
  `RaptorTwentyOne/Anabolic`. The request carries nothing about this PC
  beyond what any download does: its address, and `Anabolic/<version>` as
  the program asking. Installing an update downloads the release zip from
  GitHub. Settings → Updates → *Don't check* turns both off.
- **Names of remote addresses.** The Network page asks the system's DNS
  resolver for the names of the addresses programs talk to, as any reverse
  lookup does.
- **Links you click.** Looking a file up on VirusTotal, an address on
  ipinfo.io or a name on a search engine opens your browser at that site,
  and only when you ask. Anabolic itself sends nothing to them.

Nothing else: no usage statistics, no crash reports, no identifiers.

## What stays on the computer

- `%APPDATA%\Anabolic` (Windows), `~/.local/share/anabolic` (Linux):
  settings.
- `%LOCALAPPDATA%\Anabolic` (Windows), `~/.local/share/anabolic` (Linux):
  - the history: every 10 seconds while Anabolic runs, the machine's
    totals and the processes that led, started or exited, with their
    names. It is kept for the days chosen in Settings (7 by default) and
    can be turned off there;
  - Activity recordings, memory dumps and exports, only when you make
    them. Recordings list the files, registry keys and connections a
    program used; a full memory dump holds everything the process had in
    memory, passwords and keys included;
  - `stalls.log`: the moments Anabolic's own window fell behind.

Delete these folders to remove all of it.

## The MCP server

The MCP server is off until you turn it on. It listens on `127.0.0.1`
only, so other computers cannot reach it, and every request must carry the
key shown on the MCP page.

What a connected program can do is what you allow there: reading the
machine's state, recording activity, acting on processes and services,
writing memory dumps. Each kind can be turned off. Whatever a tool returns
(process names and command lines, file paths, connections, recordings) goes
to the program that called it, and from there to whatever service that
program uses, such as an AI model's provider. Their privacy policies apply
to it from then on.

## Contact

Questions and reports: open an issue at
<https://github.com/RaptorTwentyOne/Anabolic/issues>.
