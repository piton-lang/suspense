//! The controlled piton command, as the SandboxScope's piton says: `piton`
//! on a run's PATH is a small shim, the application's own program run as
//! `suspense --piton-shim`, which asks the application, over a Unix socket
//! in the run's temporary directory made for that run alone, or on Windows a
//! named pipe only the run's AppContainer and the user can open, to run the
//! piton command for it, on the host, as trusted work, and streams back what
//! it prints and its exit status. Only what the run's profile allows is run.
//!
//! The protocol: the shim writes its arguments as a JSON array and a line
//! break; the application answers with frames, a tag byte, `o` for standard
//! output, `e` for standard error, then a four-byte big-endian length and
//! the bytes; and last `x` with a four-byte big-endian exit status.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use super::ProfileKind;

/// The environment variable the shim finds its channel by.
pub const SOCKET_ENV: &str = "SUSPENSE_PITON_CHANNEL";

/// The argument the application is run with to be the shim.
pub const SHIM_ARG: &str = "--piton-shim";

/// Whether a run under `kind` may run piton with `args`: a Spec run may
/// build, check, format, and slice; a Code or Question run may only slice
/// with an adapter, which prints compiled output, never the source.
pub fn allowed(kind: ProfileKind, args: &[String]) -> Result<(), String> {
    let command = args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .map(String::as_str);
    let adapter = args
        .iter()
        .any(|arg| arg == "--adapter" || arg.starts_with("--adapter="));
    let ok = match kind {
        ProfileKind::Spec | ProfileKind::Freeform => {
            matches!(
                command,
                Some("build" | "check" | "fmt" | "format" | "slice")
            ) || args.iter().any(|arg| arg == "--version" || arg == "--help")
        }
        ProfileKind::Code | ProfileKind::Question => {
            (command == Some("slice") && adapter)
                || args.iter().any(|arg| arg == "--version" || arg == "--help")
        }
        ProfileKind::Bare => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "piton {} isn't allowed in a {} run's sandbox: {}\n",
            args.join(" "),
            kind.label(),
            match kind {
                ProfileKind::Code | ProfileKind::Question =>
                    "only `piton slice --adapter <adapter> <target>` is, which prints compiled output",
                _ => "only build, check, fmt, and slice are",
            }
        ))
    }
}

/// A run's piton channel, listening until it is dropped.
pub struct Server {
    address: PathBuf,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Server {
    /// Listens for the run of `kind`, named `label`, in the project at
    /// `project_dir`, in its temporary directory `temp`; with the folder
    /// holding its `piton` shim, to put first on its PATH.
    #[cfg(unix)]
    pub fn start(
        kind: ProfileKind,
        project_dir: &Path,
        temp: &Path,
        _label: &str,
    ) -> Result<(Self, PathBuf)> {
        use std::os::unix::net::UnixListener;
        let address = temp.join("piton.sock");
        let listener = UnixListener::bind(&address).context("could not listen for piton")?;
        let bin = temp.join("bin");
        std::fs::create_dir_all(&bin)?;
        let exe = std::env::current_exe().context("could not find the application")?;
        let shim = bin.join("piton");
        let quoted = exe.to_string_lossy().replace('\'', "'\\''");
        std::fs::write(
            &shim,
            format!("#!/bin/sh\nexec '{quoted}' {SHIM_ARG} \"$@\"\n"),
        )?;
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))?;
        }
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let project_dir = project_dir.to_path_buf();
        std::thread::spawn({
            let stop = stop.clone();
            move || {
                for stream in listener.incoming() {
                    if stop.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else {
                        continue;
                    };
                    let Ok(reader) = stream.try_clone() else {
                        continue;
                    };
                    let project_dir = project_dir.clone();
                    std::thread::spawn(move || serve(kind, &project_dir, reader, stream));
                }
            }
        });
        Ok((Self { address, stop }, bin))
    }

    #[cfg(windows)]
    pub fn start(
        kind: ProfileKind,
        project_dir: &Path,
        temp: &Path,
        label: &str,
    ) -> Result<(Self, PathBuf)> {
        let address = PathBuf::from(format!(r"\\.\pipe\suspense-{label}"));
        let bin = temp.join("bin");
        std::fs::create_dir_all(&bin)?;
        let exe = std::env::current_exe().context("could not find the application")?;
        std::fs::write(
            bin.join("piton.cmd"),
            format!("@\"{}\" {SHIM_ARG} %*\r\n", exe.display()),
        )?;
        // The first instance is made here, so a pipe that can't be made
        // refuses the run rather than its piton.
        let first = super::windows::pipe(&address.to_string_lossy(), label, false)
            .context("could not listen for piton")?;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let project_dir = project_dir.to_path_buf();
        std::thread::spawn({
            let stop = stop.clone();
            let name = address.to_string_lossy().into_owned();
            let label = label.to_string();
            move || {
                let mut next = Some(first);
                loop {
                    let pipe = match next.take() {
                        Some(pipe) => super::windows::connect(pipe),
                        None => super::windows::pipe(&name, &label, true)
                            .and_then(super::windows::connect),
                    };
                    if stop.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    let Ok(pipe) = pipe else {
                        break;
                    };
                    let Ok(reader) = pipe.try_clone() else {
                        continue;
                    };
                    let project_dir = project_dir.clone();
                    std::thread::spawn(move || serve(kind, &project_dir, reader, pipe));
                }
            }
        });
        Ok((Self { address, stop }, bin))
    }

    pub fn address(&self) -> &Path {
        &self.address
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        // Wakes the listener, so it sees it is to stop.
        #[cfg(unix)]
        {
            std::os::unix::net::UnixStream::connect(&self.address).ok();
            std::fs::remove_file(&self.address).ok();
        }
        #[cfg(windows)]
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.address)
            .ok();
    }
}

fn frame(out: &mut impl std::io::Write, tag: u8, bytes: &[u8]) -> std::io::Result<()> {
    out.write_all(&[tag])?;
    out.write_all(&(bytes.len() as u32).to_be_bytes())?;
    out.write_all(bytes)
}

/// Answers one request: refused, or run on the host, its output streamed.
fn serve(
    kind: ProfileKind,
    project_dir: &Path,
    reader: impl std::io::Read,
    mut writer: impl std::io::Write,
) {
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let args: Vec<String> = serde_json::from_str(line.trim()).unwrap_or_default();
    if let Err(why) = allowed(kind, &args) {
        frame(&mut writer, b'e', why.as_bytes()).ok();
        frame(&mut writer, b'x', &2i32.to_be_bytes()[..]).ok();
        return;
    }
    crate::debug_log::log(
        Some(project_dir),
        format!("piton {} for a {} run", args.join(" "), kind.label()),
    );
    let child = crate::process::command("piton")
        .args(&args)
        .current_dir(project_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(err) => {
            frame(
                &mut writer,
                b'e',
                format!("piton couldn't be run: {err}\n").as_bytes(),
            )
            .ok();
            frame(&mut writer, b'x', &127i32.to_be_bytes()[..]).ok();
            return;
        }
    };
    let (tx, rx) = std::sync::mpsc::channel::<(u8, Vec<u8>)>();
    let pump = |mut from: Box<dyn std::io::Read + Send>,
                tag: u8,
                tx: std::sync::mpsc::Sender<(u8, Vec<u8>)>| {
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            while let Ok(read) = from.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                tx.send((tag, buffer[..read].to_vec())).ok();
            }
        })
    };
    let out = child
        .stdout
        .take()
        .map(|out| pump(Box::new(out), b'o', tx.clone()));
    let err = child
        .stderr
        .take()
        .map(|err| pump(Box::new(err), b'e', tx.clone()));
    drop(tx);
    for (tag, bytes) in rx {
        if frame(&mut writer, tag, &bytes).is_err() {
            child.kill().ok();
            break;
        }
    }
    out.map(|out| out.join());
    err.map(|err| err.join());
    let code = child
        .wait()
        .ok()
        .and_then(|status| status.code())
        .unwrap_or(1);
    frame(&mut writer, b'x', &code.to_be_bytes()[..]).ok();
}

/// The shim: asks the application to run piton with `args`, printing what
/// it prints, and gives its exit status.
pub fn shim(args: Vec<String>) -> i32 {
    let Some(address) = std::env::var_os(SOCKET_ENV) else {
        eprintln!("piton: this run has no piton channel");
        return 127;
    };
    #[cfg(unix)]
    let stream = std::os::unix::net::UnixStream::connect(&address);
    #[cfg(windows)]
    let stream = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&address);
    #[cfg(not(any(unix, windows)))]
    let stream: std::io::Result<std::fs::File> =
        Err(std::io::Error::other("this platform has no piton channel"));
    let mut stream = match stream {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("piton: couldn't reach the application: {err}");
            return 127;
        }
    };
    let request = serde_json::to_string(&args).unwrap_or_else(|_| "[]".into());
    if stream.write_all(format!("{request}\n").as_bytes()).is_err() {
        return 127;
    }
    let (mut stdout, mut stderr) = (std::io::stdout(), std::io::stderr());
    loop {
        let mut head = [0u8; 5];
        if stream.read_exact(&mut head).is_err() {
            return 1;
        }
        let length = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
        let mut body = vec![0u8; length];
        if stream.read_exact(&mut body).is_err() {
            return 1;
        }
        match head[0] {
            b'o' => {
                stdout.write_all(&body).ok();
                stdout.flush().ok();
            }
            b'e' => {
                stderr.write_all(&body).ok();
            }
            b'x' => {
                return i32::from_be_bytes([body[0], body[1], body[2], body[3]]);
            }
            _ => return 1,
        }
    }
}
