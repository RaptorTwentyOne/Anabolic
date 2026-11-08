# anabolic-core

The system side of [Anabolic](https://crates.io/crates/anabolic), a
performance monitoring and diagnostics tool for Windows and Linux: process
scans, kernel traces (ETW on Windows, perf events on Linux), handles,
services, disks and their health, boots, the history, and the actions on
processes and services. It has no interface; the `anabolic` crate is the
application.

It is published so that `anabolic` can be installed with cargo. Its API
follows what the application needs and changes with it, with no stability
promise between versions.

Source, issues and the full description:
<https://github.com/RaptorTwentyOne/Anabolic>.

## License

MIT or Apache-2.0, at your option.
