//! Device nodes a program opens by path (the kernel's pseudo-filesystem, not
//! the image).

/// The kernel console: a line written to it reaches the serial port even when
/// the writer's stdout is a Terminal window's pty (the UI probe and the
/// LazyRAD player's markers use it).
pub const CONSOLE: &str = "/dev/console";
