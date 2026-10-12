//! The Linux backend, as the SandboxScope says: Landlock, applied in the
//! child just before it runs the harness's program, `no_new_privs` set
//! first, so it is never applied to Suspense itself and needs no root. The
//! ruleset is built, every path opened as a file descriptor, before the
//! child is forked, so the child only restricts itself.

use std::os::unix::process::CommandExt as _;
use std::process::Command;
use std::sync::Mutex;

use landlock::{
    ABI, Access as _, AccessFs, CompatLevel, Compatible as _, PathBeneath, PathFd, Ruleset,
    RulesetAttr as _, RulesetCreatedAttr as _, RulesetStatus, Scope,
};
use std::os::fd::{AsFd as _, AsRawFd as _};

use super::{Access, Compiled, Refused};

/// The Landlock ABI the kernel gives, 0 where it gives none.
fn kernel_abi() -> i32 {
    // landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)
    // gives the ABI version.
    let version = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            1u32,
        )
    };
    version.max(0) as i32
}

/// What an open descriptor is, its device and inode, and whether it is a
/// folder.
fn identity(fd: &PathFd) -> (Option<(u64, u64)>, bool) {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd.as_fd().as_raw_fd(), &mut stat) } != 0 {
        return (None, false);
    }
    (
        Some((stat.st_dev as u64, stat.st_ino as u64)),
        (stat.st_mode & libc::S_IFMT) == libc::S_IFDIR,
    )
}

/// The least ABI the sandbox runs on: 3, so truncating is covered too.
const LEAST_ABI: i32 = 3;

/// The ABI scoping abstract Unix sockets and signals.
const SCOPED_ABI: i32 = 6;

/// The ABI covering connecting to Unix sockets by path.
const RESOLVE_UNIX_ABI: i32 = 9;

/// The ABI used, the kernel's, but no newer than this crate knows.
fn abi() -> (i32, ABI) {
    let version = kernel_abi();
    let known = version.min(9);
    (version, ABI::from(known))
}

pub fn status() -> Result<String, Refused> {
    let (version, _) = abi();
    if version == 0 {
        return Err(Refused {
            message: "this kernel has no Landlock, which the sandbox needs: it isn't built \
                      with it, or it isn't enabled at boot (add `landlock` to the `lsm=` \
                      kernel parameter)"
                .into(),
            details: "landlock_create_ruleset gave no ABI version".into(),
        });
    }
    if version < LEAST_ABI {
        return Err(Refused {
            message: format!(
                "this kernel's Landlock is ABI {version}, and the sandbox needs ABI {LEAST_ABI} \
                 or later, as Linux 6.2 gives"
            ),
            details: String::new(),
        });
    }
    let reach = if version >= RESOLVE_UNIX_ABI {
        "files and local sockets"
    } else if version >= SCOPED_ABI {
        "files and some local sockets"
    } else {
        "files only, not local sockets"
    };
    Ok(format!("Landlock ABI {version}: {reach}"))
}

pub fn enforce(command: &mut Command, compiled: &Compiled) -> Result<(), Refused> {
    status()?;
    let (version, abi) = abi();
    let failed = |err: landlock::RulesetError| Refused {
        message: "its Landlock ruleset couldn't be built".into(),
        details: err.to_string(),
    };
    let all = AccessFs::from_all(abi);
    let read = AccessFs::from_read(abi);
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(all)
        .map_err(failed)?;
    if version >= SCOPED_ABI {
        ruleset = ruleset
            .scope(Scope::AbstractUnixSocket | Scope::Signal)
            .map_err(failed)?;
    }
    let mut created = ruleset.create().map_err(failed)?;
    for rule in &compiled.rules {
        let access = match rule.access {
            Access::ReadOnly => read,
            Access::ReadWrite => all,
        };
        // Opened now, as a descriptor: the grant is on what it is, never on
        // its name looked up again, and only where it is still what the
        // policy was compiled from.
        let Ok(fd) = PathFd::new(&rule.path) else {
            continue;
        };
        let (opened, is_dir) = identity(&fd);
        if rule.id.is_some() && opened != rule.id {
            return Err(Refused {
                message: format!(
                    "{} changed between checking the policy and granting it",
                    rule.path.display()
                ),
                details: String::new(),
            });
        }
        let access = if is_dir {
            access
        } else {
            access & AccessFs::from_file(abi)
        };
        created = created
            .add_rule(PathBeneath::new(fd, access))
            .map_err(|err| Refused {
                message: format!("{} couldn't be granted", rule.path.display()),
                details: err.to_string(),
            })?;
    }
    let created = Mutex::new(Some(created));
    // In the child, after fork, before exec: nothing but syscalls.
    unsafe {
        command.pre_exec(move || {
            // Every descriptor but the standard three closed as it runs its
            // program, so none open on a denied file is inherited.
            libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            );
            let ruleset = created
                .lock()
                .ok()
                .and_then(|mut created| created.take())
                .ok_or_else(|| std::io::Error::other("the sandbox was applied twice"))?;
            let status = ruleset.restrict_self().map_err(std::io::Error::other)?;
            if status.ruleset != RulesetStatus::FullyEnforced || !status.no_new_privs {
                return Err(std::io::Error::other(
                    "the sandbox couldn't be fully enforced",
                ));
            }
            Ok(())
        });
    }
    Ok(())
}
