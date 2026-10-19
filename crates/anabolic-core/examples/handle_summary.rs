//! Handle counts by object type for a PID, read from the system handle table
//! (the target process is not opened). For process handles it also works out
//! how many distinct processes are referenced and how many of those exited.

#[cfg(windows)]
#[path = "windows/handle_summary.rs"]
mod example;

fn main() {
    #[cfg(windows)]
    example::main();
    #[cfg(not(windows))]
    eprintln!("This example reads the Windows system handle table: it runs on Windows only.");
}
