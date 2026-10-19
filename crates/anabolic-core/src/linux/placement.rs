//! Window placement is the compositor's on Wayland: a program cannot see
//! where other programs' windows are, nor move its own. There is no
//! full-screen game to keep away from as far as Anabolic can tell.

/// The monitor a full-screen game fills: never known here.
pub fn fullscreen_monitor() -> Option<isize> {
    None
}
