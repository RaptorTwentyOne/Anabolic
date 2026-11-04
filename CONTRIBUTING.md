# Contributing to Anabolic

Bug reports, ideas and pull requests are all welcome. This page says where
each goes and what keeps them easy to act on.

## Questions and ideas

Ask in [Discussions](https://github.com/RaptorTwentyOne/Anabolic/discussions):
how something works, whether Anabolic can do something, an idea that is
not yet a plan. A feature request with a clear case goes in an
[issue](https://github.com/RaptorTwentyOne/Anabolic/issues/new/choose).

## Reporting a bug

Use the bug report form. What makes a bug quick to fix:

- the version (Settings → About) and the system (Windows build, or the
  Linux distribution and desktop);
- whether Anabolic ran as an administrator (on Linux, with full access);
- what you did, what happened and what you expected;
- for a window that froze or fell behind, the last lines of `stalls.log`
  in Anabolic's data folder (see [Privacy](README.md#privacy)).

Screenshots, exports and dumps show process names, command lines and your
user name. Look at them before attaching, and blur what is private.

A security problem goes to [SECURITY.md](SECURITY.md) instead, not to a
public issue.

## Building and testing

[Building from source](README.md#building-from-source) in the README has
the toolchain and the layout. Before a pull request, run what CI runs on
Windows and on Linux:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Tests that need administrator rights (on Linux, full access) skip
themselves without them; run them elevated too when your change touches
what they cover. The workspace also has to build with Rust 1.95, the
oldest version it claims.

## Pull requests

- For more than a small fix, open an issue or a discussion first, so that
  the work is not spent on something that will not be merged.
- One change per pull request. Say what it changes and why, and how you
  tried it: on which system, as an administrator or not.
- New behaviour comes with tests where it can: the core crate's logic
  usually can, the interface rarely.
- Say in the pull request what a user would notice; the changelog is
  written from that when the release is made.

## What the code holds to

These are why Anabolic exists, and changes that break them are not
merged, however useful otherwise:

- **The interface thread never waits.** Nothing that can block (opening a
  process, a lock another thread holds, the disk, the network) runs on it.
  The collector reads the system once per refresh; slower work goes to its
  own thread and reports back.
- **The regular views open no process.** They read system-wide tables and
  kernel traces. What opens a process (handles, modules, thread stacks,
  dumps) happens when the user asks, and asks first for games and
  anti-cheat programs. Game mode and rules never touch a game, Windows'
  critical processes or anti-cheat software.
- **Nothing leaves the PC** except the update check and what the user
  opens in the browser. No telemetry.
- **Both systems.** Each core module says what it means on every system;
  what reads it lives in `windows/` and `linux/`. A feature one system
  cannot have says so there, and why.
- **Comments say why**, and interface text says what happens in plain
  words.

## License

Unless you say otherwise, what you contribute is licensed as the rest of
Anabolic: under either the MIT or the Apache 2.0 license (see
[License](README.md#license)). Taking part in this project means keeping to
its [code of conduct](CODE_OF_CONDUCT.md).
