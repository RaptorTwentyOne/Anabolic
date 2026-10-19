//! One Anabolic per session. The first instance claims the session's
//! activation point and waits on it; a later one (a second double-click, a
//! shortcut clicked while the first is still starting) signals it and
//! exits, and the first brings its window to the front.
//!
//! The claim is made before anything else happens at startup, so two
//! instances starting together cannot both miss each other the way a search
//! for an existing window could.

#[cfg_attr(windows, path = "windows/instance.rs")]
#[cfg_attr(target_os = "linux", path = "linux/instance.rs")]
mod imp;
pub use imp::*;

pub enum Claim {
    /// No other instance: serve activations with this.
    First(Activation),
    /// Another instance runs and has been asked to come to the front.
    Running,
    /// The claim could not be made; run without single-instance.
    Unavailable,
}
