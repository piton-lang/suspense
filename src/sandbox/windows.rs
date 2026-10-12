//! The Windows backend, as the SandboxScope's Windows says: AppContainer.
//!
//! Each run gets an AppContainer of its own, `Suspense.<profile>.<run id>`,
//! made before it starts and deleted after. The filesystem boundary is the
//! access lists: each path the policy grants is given an access entry for
//! that AppContainer's SID alone, read and execute or modify, recorded first
//! in a ledger so it is always removed again, after the run or at the next
//! launch. The harness is started by the application's own program run as
//! `suspense --sandbox-launch <spec> -- <program>…`, which makes the
//! AppContainer and the grants, starts the harness suspended in it with only
//! the `internetClient` capability and its standard handles, puts it in a
//! job object that ends everything with it and forbids breaking away, and
//! only then resumes it.
//!
//! This backend has not run on Windows yet: it type-checks against the
//! Windows target, and is "in theory" until its tests pass on a Windows
//! runner, as SANDBOX.md says.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, HANDLE_FLAG_INHERIT,
    INVALID_HANDLE_VALUE, LocalFree, SetHandleInformation,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, REVOKE_ACCESS,
    SDDL_REVISION_1, SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
    TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    ACL, ACL_REVISION, CreateWellKnownSid, DACL_SECURITY_INFORMATION, FreeSid,
    GetSecurityDescriptorSacl, GetTokenInformation, InitializeAcl, LABEL_SECURITY_INFORMATION,
    NO_INHERITANCE, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES,
    SID_AND_ATTRIBUTES, SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_QUERY, TOKEN_USER, TokenUser,
    WinCapabilityInternetClientSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_FIRST_PIPE_INSTANCE,
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, GetFileAttributesW,
    INVALID_FILE_ATTRIBUTES, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    OpenProcessToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};

use super::{Access, Compiled, Refused};

/// The argument the application is run with to launch a sandboxed run.
pub const LAUNCH_ARG: &str = "--sandbox-launch";

/// The first Windows build with what AppContainer sandboxing needs: 1809.
const LEAST_BUILD: u32 = 17763;

/// A group in a token that is in force.
const SE_GROUP_ENABLED: u32 = 4;

/// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`.
const ALREADY_EXISTS: i32 = 0x800700B7_u32 as i32;

fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn failed(message: impl Into<String>) -> Refused {
    Refused {
        message: message.into(),
        details: format!("Windows error {}", unsafe { GetLastError() }),
    }
}

/// The Windows build number, as the registry holds it.
fn build() -> Option<u32> {
    let key = wide("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
    let value = wide("CurrentBuildNumber");
    let mut buffer = [0u16; 32];
    let mut size = (buffer.len() * 2) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if result != 0 {
        return None;
    }
    let text = String::from_utf16_lossy(&buffer[..(size as usize / 2).saturating_sub(1)]);
    text.trim().parse().ok()
}

pub fn status() -> Result<String, Refused> {
    match build() {
        Some(build) if build >= LEAST_BUILD => Ok("AppContainer: files and named pipes".into()),
        Some(build) => Err(Refused {
            message: format!(
                "this Windows is build {build}, and the sandbox needs Windows 10 version 1809 \
                 (build {LEAST_BUILD}) or later"
            ),
            details: String::new(),
        }),
        None => Err(Refused {
            message:
                "this Windows's version couldn't be read, so the sandbox can't tell it can run"
                    .into(),
            details: String::new(),
        }),
    }
}

/// The AppContainer SID of `name`, as a string; none where it can't be
/// derived.
pub fn container_sid(name: &str) -> Option<String> {
    let name = wide(name);
    let mut sid: PSID = std::ptr::null_mut();
    if unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) } < 0 {
        return None;
    }
    let text = sid_string(sid);
    unsafe { FreeSid(sid) };
    text
}

fn sid_string(sid: PSID) -> Option<String> {
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return None;
    }
    let length = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
    let string = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    unsafe { LocalFree(text.cast()) };
    Some(string)
}

/// The signed-in user's SID, as a string.
pub fn user_sid() -> Option<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let mut buffer = vec![0u8; 256];
    let mut length = 0u32;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut length,
        )
    };
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return None;
    }
    let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
    sid_string(user.User.Sid)
}

/// What the launcher is given: the run's AppContainer, what to grant, and
/// the ledger to record each grant in first.
#[derive(Debug, Serialize, Deserialize)]
struct LaunchSpec {
    container: String,
    grants: Vec<(PathBuf, bool)>,
    ledger: PathBuf,
}

/// One grant recorded in a ledger: its path, the SID it was made to, and
/// whether the path was labelled low integrity for it to be written.
#[derive(Debug, Serialize, Deserialize)]
struct Granted {
    path: PathBuf,
    sid: String,
    #[serde(default)]
    low: bool,
}

/// Where the ledgers are kept: Suspense's data directory.
fn ledgers() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("suspense")
        .join("sandbox-ledger")
}

fn ledger(container: &str) -> PathBuf {
    ledgers().join(format!("{container}.jsonl"))
}

/// Puts the sandbox on `command`: started through the launcher, which makes
/// the run's AppContainer and grants before the harness runs in it.
pub fn enforce(command: &mut Command, compiled: &Compiled) -> Result<(), Refused> {
    status()?;
    if compiled.label.is_empty() {
        return Err(Refused {
            message: "the run has no AppContainer named for it".into(),
            details: String::new(),
        });
    }
    let spec = LaunchSpec {
        container: compiled.label.clone(),
        grants: compiled
            .rules
            .iter()
            .map(|rule| (rule.path.clone(), rule.access == Access::ReadWrite))
            .collect(),
        ledger: ledger(&compiled.label),
    };
    let file = std::env::temp_dir().join(format!("{}.launch.json", compiled.label));
    let text = serde_json::to_string(&spec).map_err(|err| Refused {
        message: "the run's launch couldn't be written".into(),
        details: err.to_string(),
    })?;
    std::fs::write(&file, text).map_err(|err| Refused {
        message: "the run's launch couldn't be written".into(),
        details: err.to_string(),
    })?;
    let exe = std::env::current_exe().map_err(|err| Refused {
        message: "the application couldn't find itself to launch the run".into(),
        details: err.to_string(),
    })?;
    let program = command.get_program().to_owned();
    let args: Vec<std::ffi::OsString> = command.get_args().map(ToOwned::to_owned).collect();
    let envs: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)> = command
        .get_envs()
        .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
        .collect();
    let dir = command.get_current_dir().map(ToOwned::to_owned);
    let mut wrapped = crate::process::command(exe);
    wrapped
        .arg(LAUNCH_ARG)
        .arg(&file)
        .arg("--")
        .arg(program)
        .args(args);
    for (key, value) in envs {
        match value {
            Some(value) => wrapped.env(key, value),
            None => wrapped.env_remove(key),
        };
    }
    if let Some(dir) = dir {
        wrapped.current_dir(dir);
    }
    *command = wrapped;
    Ok(())
}

/// Gives `sid` `access` to `path`, or with none, takes away what it was
/// given, the change carried down to everything beneath it.
fn set_access(path: &Path, sid: &str, access: Option<bool>) -> Result<(), Refused> {
    let name = wide(path);
    // Never through a link: a reparse point is granted as nothing.
    let attributes = unsafe { GetFileAttributesW(name.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        return Ok(());
    }
    if access.is_some() && attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(Refused {
            message: format!("{} is a link, which is never granted", path.display()),
            details: String::new(),
        });
    }
    let folder = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    let mut sid_value: PSID = std::ptr::null_mut();
    let sid_text = wide(sid);
    if unsafe { ConvertStringSidToSidW(sid_text.as_ptr(), &mut sid_value) } == 0 {
        return Err(failed(format!(
            "the run's identity couldn't be read to grant {}",
            path.display()
        )));
    }
    let mut old: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let read = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut old,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if read != 0 {
        unsafe { LocalFree(sid_value) };
        return Err(Refused {
            message: format!("the access list of {} couldn't be read", path.display()),
            details: format!("Windows error {read}"),
        });
    }
    let permissions = match access {
        Some(true) => FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE,
        _ => FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
    };
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: permissions,
        grfAccessMode: if access.is_some() {
            GRANT_ACCESS
        } else {
            REVOKE_ACCESS
        },
        grfInheritance: if folder {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        },
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid_value.cast(),
        },
    };
    let mut new: *mut ACL = std::ptr::null_mut();
    let merged = unsafe { SetEntriesInAclW(1, &entry, old, &mut new) };
    let result = if merged != 0 {
        Err(Refused {
            message: format!("the access list of {} couldn't be changed", path.display()),
            details: format!("Windows error {merged}"),
        })
    } else {
        // By name, so the change is carried down to what it holds.
        let set = unsafe {
            SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                new,
                std::ptr::null(),
            )
        };
        if set != 0 {
            Err(Refused {
                message: format!(
                    "{} couldn't be granted: its access list can't be changed by you",
                    path.display()
                ),
                details: format!("Windows error {set}"),
            })
        } else {
            Ok(())
        }
    };
    unsafe {
        if !new.is_null() {
            LocalFree(new.cast());
        }
        LocalFree(descriptor);
        LocalFree(sid_value);
    }
    result
}

/// Labels `path` low integrity, carried down to what it holds, so a run,
/// which runs at low integrity as every AppContainer does, can write it; or
/// with `low` false, takes the label away again. The label lets nothing
/// that its access list doesn't.
fn set_low(path: &Path, low: bool) -> Result<(), Refused> {
    let name = wide(path);
    let attributes = unsafe { GetFileAttributesW(name.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        return Ok(());
    }
    let folder = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let mut empty = [0u64; 4];
    let sacl: *mut ACL = if low {
        let sddl = wide(if folder {
            "S:(ML;OICI;NW;;;LW)"
        } else {
            "S:(ML;;NW;;;LW)"
        });
        let (mut present, mut defaulted, mut sacl) = (0, 0, std::ptr::null_mut());
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
            || unsafe {
                GetSecurityDescriptorSacl(descriptor, &mut present, &mut sacl, &mut defaulted)
            } == 0
        {
            return Err(failed(format!(
                "{} couldn't be labelled to be written",
                path.display()
            )));
        }
        sacl
    } else {
        let acl: *mut ACL = empty.as_mut_ptr().cast();
        if unsafe { InitializeAcl(acl, std::mem::size_of_val(&empty) as u32, ACL_REVISION) } == 0 {
            return Err(failed(format!(
                "{}'s label couldn't be taken away",
                path.display()
            )));
        }
        acl
    };
    let set = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
            sacl,
        )
    };
    if !descriptor.is_null() {
        unsafe { LocalFree(descriptor) };
    }
    if set != 0 {
        return Err(Refused {
            message: format!("{} couldn't be labelled to be written", path.display()),
            details: format!("Windows error {set}"),
        });
    }
    Ok(())
}

/// Removes every grant the ledger of `container` records, and the
/// AppContainer itself, then the ledger: after a run, however it ended.
pub fn cleanup(container: &str) {
    let file = ledger(container);
    if let Ok(text) = std::fs::read_to_string(&file) {
        for line in text.lines() {
            if let Ok(granted) = serde_json::from_str::<Granted>(line) {
                set_access(&granted.path, &granted.sid, None).ok();
                if granted.low {
                    set_low(&granted.path, false).ok();
                }
            }
        }
    }
    let name = wide(container);
    unsafe { DeleteAppContainerProfile(name.as_ptr()) };
    std::fs::remove_file(file).ok();
}

/// Removes what any run the application didn't see end left in place, as
/// after a crash: called at launch, when no run is under way.
pub fn cleanup_left() {
    let Ok(entries) = std::fs::read_dir(ledgers()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "jsonl")
            && let Some(container) = path.file_stem().and_then(|stem| stem.to_str())
        {
            cleanup(container);
        }
    }
}

/// Quotes `arg` for a Windows command line.
fn quoted(arg: &OsStr) -> String {
    let arg = arg.to_string_lossy();
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.into_owned();
    }
    let mut out = String::from('"');
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            c => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

/// The launcher: `suspense --sandbox-launch <spec> -- <program> <args>…`.
/// Makes the run's AppContainer and grants, runs the program in it, in a job
/// of its own, waits for it, and takes everything away again. Gives its
/// exit status.
pub fn launch(args: &[String]) -> i32 {
    match launched(args) {
        Ok(code) => code as i32,
        Err(refused) => {
            eprintln!("{}", refused);
            1
        }
    }
}

fn launched(args: &[String]) -> Result<u32, Refused> {
    let (Some(spec), Some(rest)) = (args.first(), args.get(1..)) else {
        return Err(failed("the launch was given nothing to run"));
    };
    let program: Vec<&String> = rest.iter().skip_while(|arg| *arg == "--").collect();
    if program.is_empty() {
        return Err(failed("the launch was given nothing to run"));
    }
    status()?;
    let spec: LaunchSpec = std::fs::read_to_string(spec)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .ok_or_else(|| failed("the run's launch couldn't be read"))?;
    std::fs::remove_file(args[0].as_str()).ok();
    let result = run_contained(&spec, &program);
    cleanup(&spec.container);
    result
}

fn run_contained(spec: &LaunchSpec, program: &[&String]) -> Result<u32, Refused> {
    // The run's own identity.
    let name = wide(&spec.container);
    let mut sid: PSID = std::ptr::null_mut();
    let made = unsafe {
        CreateAppContainerProfile(
            name.as_ptr(),
            name.as_ptr(),
            name.as_ptr(),
            std::ptr::null(),
            0,
            &mut sid,
        )
    };
    if made == ALREADY_EXISTS {
        if unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) } < 0 {
            return Err(failed("the run's AppContainer couldn't be found"));
        }
    } else if made < 0 {
        return Err(Refused {
            message: "the run's AppContainer couldn't be made".into(),
            details: format!("HRESULT {made:#x}"),
        });
    }
    let sid_text = sid_string(sid).ok_or_else(|| failed("the run's identity couldn't be read"))?;
    // Each grant recorded first, so it is always taken away again.
    std::fs::create_dir_all(spec.ledger.parent().unwrap_or(Path::new("."))).ok();
    for (path, write) in &spec.grants {
        let line = serde_json::to_string(&Granted {
            path: path.clone(),
            sid: sid_text.clone(),
            low: *write,
        })
        .unwrap_or_default();
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&spec.ledger)
            .and_then(|mut ledger| writeln!(ledger, "{line}"))
            .map_err(|err| Refused {
                message: "a grant couldn't be recorded, so it isn't made".into(),
                details: err.to_string(),
            })?;
        set_access(path, &sid_text, Some(*write))?;
        if *write {
            set_low(path, true)?;
        }
    }
    // Only internetClient, so the harness can reach its model service.
    let mut capability = [0u8; 68];
    let mut size = capability.len() as u32;
    if unsafe {
        CreateWellKnownSid(
            WinCapabilityInternetClientSid,
            std::ptr::null_mut(),
            capability.as_mut_ptr().cast(),
            &mut size,
        )
    } == 0
    {
        return Err(failed("the internetClient capability couldn't be given"));
    }
    let mut capabilities = [SID_AND_ATTRIBUTES {
        Sid: capability.as_mut_ptr().cast(),
        Attributes: SE_GROUP_ENABLED,
    }];
    let security = SECURITY_CAPABILITIES {
        AppContainerSid: sid,
        Capabilities: capabilities.as_mut_ptr(),
        CapabilityCount: 1,
        Reserved: 0,
    };
    // Its standard handles, the only ones it inherits.
    let handles: [HANDLE; 3] = unsafe {
        [
            GetStdHandle(STD_INPUT_HANDLE),
            GetStdHandle(STD_OUTPUT_HANDLE),
            GetStdHandle(STD_ERROR_HANDLE),
        ]
    };
    for handle in handles {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(failed(
                "the run has no standard handles to give the harness",
            ));
        }
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    }
    let mut list_size = 0usize;
    unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 2, 0, &mut list_size) };
    let mut list_buffer = vec![0u8; list_size];
    let list: LPPROC_THREAD_ATTRIBUTE_LIST = list_buffer.as_mut_ptr().cast();
    if unsafe { InitializeProcThreadAttributeList(list, 2, 0, &mut list_size) } == 0 {
        return Err(failed("the harness's start couldn't be set up"));
    }
    let updated = unsafe {
        UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            (&security as *const SECURITY_CAPABILITIES).cast(),
            std::mem::size_of::<SECURITY_CAPABILITIES>(),
            std::ptr::null_mut(),
            std::ptr::null(),
        ) != 0
            && UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                std::mem::size_of_val(&handles),
                std::ptr::null_mut(),
                std::ptr::null(),
            ) != 0
    };
    if !updated {
        unsafe { DeleteProcThreadAttributeList(list) };
        return Err(failed("the harness couldn't be given its AppContainer"));
    }
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = handles[0];
    startup.StartupInfo.hStdOutput = handles[1];
    startup.StartupInfo.hStdError = handles[2];
    startup.lpAttributeList = list;
    // A batch file, as npm installs a harness, runs through cmd.
    let first = PathBuf::from(program[0].as_str());
    let resolved = crate::programs::find(program[0]).unwrap_or(first);
    let batch = resolved
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
    let mut line: Vec<String> = Vec::new();
    if batch {
        line.extend(["cmd.exe".to_string(), "/d".into(), "/c".into()]);
    }
    line.push(quoted(resolved.as_os_str()));
    line.extend(
        program[1..]
            .iter()
            .map(|arg| quoted(OsStr::new(arg.as_str()))),
    );
    let mut command_line = wide(line.join(" "));
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let created = unsafe {
        CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT
                | CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | CREATE_UNICODE_ENVIRONMENT,
            std::ptr::null(),
            std::ptr::null(),
            &startup.StartupInfo,
            &mut info,
        )
    };
    unsafe { DeleteProcThreadAttributeList(list) };
    if created == 0 {
        return Err(failed(format!(
            "{} couldn't be started in its AppContainer",
            program[0]
        )));
    }
    // A job of its own, ending everything with it, which nothing leaves.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let jailed = !job.is_null()
        && unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) != 0
                && AssignProcessToJobObject(job, info.hProcess) != 0
        };
    if !jailed {
        unsafe {
            windows_sys::Win32::System::Threading::TerminateProcess(info.hProcess, 1);
            CloseHandle(info.hThread);
            CloseHandle(info.hProcess);
            if !job.is_null() {
                CloseHandle(job);
            }
        }
        return Err(failed("the harness couldn't be put in its job"));
    }
    unsafe { ResumeThread(info.hThread) };
    unsafe { WaitForSingleObject(info.hProcess, INFINITE) };
    let mut code = 1u32;
    unsafe {
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hThread);
        CloseHandle(info.hProcess);
        // Ends whatever it left running.
        CloseHandle(job);
    }
    Ok(code)
}

/// A named pipe instance for the piton channel at `name`, open only to the
/// run's AppContainer, by `container`, and the user, at low integrity as an
/// AppContainer runs; `more` for each after the first, which no other
/// process can have made before it.
pub fn pipe(name: &str, container: &str, more: bool) -> std::io::Result<std::fs::File> {
    use std::os::windows::io::FromRawHandle as _;
    let (Some(run), Some(user)) = (container_sid(container), user_sid()) else {
        return Err(std::io::Error::other("the pipe's owners couldn't be named"));
    };
    let sddl = wide(format!("D:P(A;;GA;;;{run})(A;;GA;;;{user})S:(ML;;NW;;;LW)"));
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let name = wide(name);
    let first = if more {
        0
    } else {
        FILE_FLAG_FIRST_PIPE_INSTANCE
    };
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | first,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            &attributes,
        )
    };
    unsafe { LocalFree(descriptor) };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_handle(handle as _) })
}

/// Waits for the shim to open `pipe`; one already open is connected.
pub fn connect(pipe: std::fs::File) -> std::io::Result<std::fs::File> {
    use std::os::windows::io::AsRawHandle as _;
    let handle = pipe.as_raw_handle() as HANDLE;
    if unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) } != 0
        || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED
    {
        Ok(pipe)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static RUNS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// A project with `allowed\note.txt`, granted read-write, beside
    /// `denied\secret.txt`, granted nothing.
    fn project() -> PathBuf {
        let n = RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("suspense-windows-test-{}-{n}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("allowed")).unwrap();
        std::fs::create_dir_all(root.join("denied")).unwrap();
        std::fs::write(root.join("allowed").join("note.txt"), "note").unwrap();
        std::fs::write(root.join("denied").join("secret.txt"), "secret").unwrap();
        root
    }

    /// Runs `cmd /d /c <line>` in an AppContainer granted `allowed`, as a
    /// run is, giving its exit status and the AppContainer's SID. Paths
    /// are left unquoted, since cmd reads no escaped quotes: the temporary
    /// folder has no spaces in it on the runners.
    fn contained(root: &Path, line: &str) -> (u32, String) {
        let n = RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let container = format!("Suspense.Test.{}-{n}", std::process::id());
        let spec = LaunchSpec {
            container: container.clone(),
            grants: vec![(root.join("allowed"), true)],
            ledger: ledger(&container),
        };
        let args: Vec<String> = ["cmd.exe", "/d", "/c", line].map(String::from).into();
        let program: Vec<&String> = args.iter().collect();
        let sid = container_sid(&container).unwrap();
        let code = run_contained(&spec, &program).unwrap();
        cleanup(&container);
        (code, sid)
    }

    fn cmd(line: &str) -> bool {
        Command::new("cmd.exe")
            .args(["/d", "/c", line])
            .status()
            .is_ok_and(|status| status.success())
    }

    #[test]
    fn a_granted_file_is_reached_and_a_denied_one_isnt() {
        let root = project();
        let note = root.join("allowed").join("note.txt");
        let secret = root.join("denied").join("secret.txt");
        assert_eq!(contained(&root, &format!("type {}", note.display())).0, 0);
        assert_ne!(contained(&root, &format!("type {}", secret.display())).0, 0);
        let written = root.join("allowed").join("written.txt");
        assert_eq!(
            contained(&root, &format!("echo x> {}", written.display())).0,
            0
        );
        let refused = root.join("denied").join("written.txt");
        assert_ne!(
            contained(&root, &format!("echo x> {}", refused.display())).0,
            0
        );
    }

    #[test]
    fn a_junction_or_a_link_into_a_denied_folder_reaches_nothing() {
        let root = project();
        let junction = root.join("allowed").join("junction");
        let denied = root.join("denied");
        assert!(cmd(&format!(
            "mklink /J {} {}",
            junction.display(),
            denied.display()
        )));
        let through = junction.join("secret.txt");
        assert_ne!(
            contained(&root, &format!("type {}", through.display())).0,
            0
        );
        // Symbolic links need developer mode; where none can be made, there
        // is none to follow.
        let link = root.join("allowed").join("link.txt");
        if std::os::windows::fs::symlink_file(denied.join("secret.txt"), &link).is_ok() {
            assert_ne!(contained(&root, &format!("type {}", link.display())).0, 0);
        }
    }

    #[test]
    fn a_hard_link_to_a_denied_file_cant_be_made() {
        let root = project();
        let link = root.join("allowed").join("hard.txt");
        let secret = root.join("denied").join("secret.txt");
        contained(
            &root,
            &format!("mklink /H {} {}", link.display(), secret.display()),
        );
        assert!(!link.exists());
    }

    #[test]
    fn a_short_name_or_a_stream_of_a_denied_file_reaches_nothing() {
        let root = project();
        let secret = root.join("denied").join("secret.txt");
        std::fs::write(format!("{}:hidden", secret.display()), "stream").unwrap();
        assert_ne!(
            contained(&root, &format!("more < {}:hidden", secret.display())).0,
            0
        );
        let long = wide(&secret);
        let mut short = vec![0u16; 1024];
        let length = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetShortPathNameW(
                long.as_ptr(),
                short.as_mut_ptr(),
                short.len() as u32,
            )
        } as usize;
        // Short names can be turned off for a volume.
        if length > 0 {
            let short = String::from_utf16_lossy(&short[..length]);
            assert_ne!(contained(&root, &format!("type {short}")).0, 0);
        }
    }

    #[test]
    fn a_process_outside_the_job_cant_be_reached() {
        let root = project();
        let me = std::process::id();
        assert_ne!(contained(&root, &format!("taskkill /PID {me}")).0, 0);
    }

    #[test]
    fn no_entry_for_the_run_remains_once_it_is_over() {
        let root = project();
        let (_, sid) = contained(&root, "exit 0");
        let listed = Command::new("icacls")
            .arg(root.join("allowed"))
            .arg("/T")
            .output()
            .unwrap();
        assert!(!String::from_utf8_lossy(&listed.stdout).contains(&sid));
    }
}
