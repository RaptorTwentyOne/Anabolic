# Security policy

Anabolic usually runs as an administrator, reads kernel traces, can open
processes, write memory dumps and end programs, and serves an MCP server.
A flaw in it can matter more than in most programs, and reports are
welcome.

## Reporting a vulnerability

Please report it privately, not in a public issue:

- **[Report a vulnerability](https://github.com/RaptorTwentyOne/Anabolic/security/advisories/new)**
  on GitHub (the Security tab's *Report a vulnerability*). Only the
  maintainer sees it, and the fix can be discussed and prepared there.
- Or by email to coskun.bagriacik@gmail.com.

Say what an attacker can do with it, what they need first (a standard
account on the PC, a program running as you, another computer on the
network) and how to reproduce it: the version (Settings → About), the
system, and the steps or a proof of concept.

You get an answer within a week. Once a fix is released, the advisory is
published with credit to you, unless you would rather not be named.

## Supported versions

The latest release. Anabolic updates itself, so fixes go out as new
releases rather than patches to older ones.

## What counts

For example:

- Something running without administrator rights gets code run with them
  through Anabolic: its files, its sign-in task (which starts it elevated
  without asking, and is only made for a copy that only administrators can
  change), its updater or its installer.
- The updater accepts a file that is not the release's: one whose SHA-256
  does not match what GitHub lists for it, or one from elsewhere.
- The MCP server can be reached from another computer, or called without
  the access key; a call does something of a kind that is turned off on
  the MCP page; or a tool acts on what it never should (Windows' critical
  processes, Anabolic itself, a dump of lsass or of the keyrings).
- On Linux, someone other than the file's owner gets the capabilities that
  *Grant full access* gives the executable.
- Memory dumps, the history or the settings readable by other users.

Not in scope: what needs administrator rights to begin with (an
administrator can already do anything Anabolic can), SmartScreen warnings
about the unsigned releases, antivirus programs flagging it, and how an
anti-cheat reacts to it.

## Checking a download

Each release file has its SHA-256 checksum beside it, as a `.sha256` file.
Releases after 0.5.0 also carry a build attestation, which proves the file
was built by this repository's release workflow:

```
gh attestation verify <file> -R RaptorTwentyOne/Anabolic
```
