//! Scripts tests write to stand in for the programs the application runs.

use std::path::Path;
use std::process::Command;

/// Makes the script at `path` executable. Written in this process, it can be
/// held open for writing by a child another test's thread forks meanwhile,
/// and running it then fails with "Text file busy". So it is replaced by a
/// copy `cp` makes in a process of its own, which no test's child holds.
pub fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut name = path.file_name().unwrap().to_os_string();
    name.push(".copy");
    let copy = path.with_file_name(name);
    let copied = Command::new("cp").arg("-p").arg(path).arg(&copy).status();
    assert!(
        copied.is_ok_and(|status| status.success()),
        "couldn't copy {path:?}"
    );
    std::fs::rename(&copy, path).unwrap();
}
