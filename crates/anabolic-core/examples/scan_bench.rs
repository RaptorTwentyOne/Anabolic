//! Steady-state cost of one NtQuerySystemInformation process scan per class.

#[cfg(windows)]
#[path = "windows/scan_bench.rs"]
mod example;

fn main() {
    #[cfg(windows)]
    example::main();
    #[cfg(not(windows))]
    eprintln!("This example times the Windows native process scan: it runs on Windows only.");
}
