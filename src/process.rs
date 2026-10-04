//! Starts the programs the application runs, as the ApplicationScope says:
//! on Windows, without a console window, since a release build there is a GUI
//! application with no console, and Windows gives every console program it
//! starts a window of its own unless told not to. Elsewhere, as ever.

use std::ffi::OsStr;
use std::process::Command;

/// Windows' `CREATE_NO_WINDOW` process creation flag.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A command running `program`, which opens no window of its own: every
/// child process the application starts is made here.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
