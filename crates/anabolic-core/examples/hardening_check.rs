//! Applies the self-hardening steps and reports which ones took effect.
use anabolic_core::hardening::{harden_process, is_elevated, own_working_set, pin_working_set};

fn main() {
    println!("elevated: {}", is_elevated());
    for item in harden_process() {
        println!("{:36} {:?}", item.what, item.result);
    }
    let ws = own_working_set() as usize;
    println!(
        "working set {} KB, pin -> {:?}",
        ws / 1024,
        pin_working_set(ws + 32 * 1024 * 1024)
    );
}
