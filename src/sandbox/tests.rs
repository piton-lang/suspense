//! The sandbox's tests, as the SandboxScope's verification says: what the
//! operating system actually does, never a mocked decision. A harness of
//! hostile commands runs inside each profile in a temporary project of its
//! own. The tests that need the platform's backend are skipped, saying so,
//! where it can't run here.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::agent::Agent;

/// A project of its own: code in `src`, spec in `spec`, the compiled
/// reference in `.claude/reference`, committed.
fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("suspense-sandbox-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("spec")).unwrap();
    std::fs::create_dir_all(dir.join(".claude/reference")).unwrap();
    std::fs::write(
        dir.join("piton.config.pi"),
        "use @piton/belay\nuse @piton/config\n\n\
         from @piton/belay import ClaudeCodeAdapter\n\n\
         export piton-config Project:\n    root: ./spec\n    frameworks:\n        - {Belay}\n\n\
         belay-config Belay:\n    codeRoot: ./src\n    adapters:\n        - {ClaudeCodeAdapter}\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/a.rs"), "fn main() {}\n").unwrap();
    std::fs::write(dir.join("spec/index.pi"), "export secret: 'spec source'\n").unwrap();
    std::fs::write(
        dir.join(".claude/reference/index.md"),
        "compiled reference\n",
    )
    .unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            // Never a repository this test was started under.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "first"]);
    // Stashed and logged too, as alternate routes to the spec.
    std::fs::write(dir.join("spec/index.pi"), "export secret: 'stashed spec'\n").unwrap();
    git(&["stash", "-q"]);
    dunce::canonicalize(&dir).unwrap()
}

/// Whether the platform's backend runs here; where it doesn't, the test
/// says so and is skipped.
fn enforced() -> bool {
    match status() {
        Ok(_) => true,
        Err(why) => {
            eprintln!("skipped: the sandbox can't run here: {}", why.message);
            false
        }
    }
}

/// Runs `script` with `sh` under `kind` in `dir`, giving its exit status
/// and what it printed, out and error together.
fn run(kind: ProfileKind, dir: &Path, script: &str) -> (i32, String) {
    let prepared = prepare(kind, Agent::Claude, dir, &RunFiles::default()).expect("no sandbox");
    let mut command = Command::new("sh");
    command.args(["-c", script]);
    prepared.apply(&mut command).expect("not applied");
    let output = command.output().expect("couldn't run");
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code().unwrap_or(-1), printed)
}

/// Whether `script` succeeds under `kind`.
fn succeeds(kind: ProfileKind, dir: &Path, script: &str) -> bool {
    let (code, printed) = run(kind, dir, script);
    if code != 0 {
        eprintln!("`{script}` under {kind:?}: {printed}");
    }
    code == 0
}

#[test]
fn policies_compile_to_an_allow_list_around_what_they_deny() {
    let dir = project("compile");
    // A grant of the whole project with the spec denied is each of its
    // entries but the spec.
    let compiled = compile(&Policy::new(&dir).read_write(&dir).deny(dir.join("spec"))).unwrap();
    let paths: Vec<&Path> = compiled
        .rules
        .iter()
        .map(|rule| rule.path.as_path())
        .collect();
    assert!(paths.contains(&dir.join("src").as_path()), "{paths:?}");
    assert!(
        !paths.iter().any(|path| path.starts_with(dir.join("spec"))),
        "{paths:?}"
    );
    assert!(
        !paths.contains(&dir.as_path()),
        "the project was granted whole"
    );
    // A grant inside a denied path is a contradiction, and refused.
    assert!(
        compile(
            &Policy::new(&dir)
                .read_only(dir.join("spec/index.pi"))
                .deny(dir.join("spec"))
        )
        .is_err()
    );
    // `..` is resolved before it is granted.
    assert!(
        compile(
            &Policy::new(&dir)
                .read_only(dir.join("src/../spec"))
                .deny(dir.join("spec"))
        )
        .is_err()
    );
    // A narrower grant of less access wins.
    let compiled = compile(
        &Policy::new(&dir)
            .read_write(&dir)
            .read_only(dir.join("src")),
    )
    .unwrap();
    let src = compiled
        .rules
        .iter()
        .find(|rule| rule.path == dir.join("src"))
        .unwrap();
    assert_eq!(src.access, Access::ReadOnly);
    std::fs::remove_dir_all(&dir).ok();
}

/// A link from a grant into a denied path, symbolic or hard, is refused
/// before anything runs.
#[cfg(unix)]
#[test]
fn links_into_denied_paths_are_refused() {
    let dir = project("links");
    std::os::unix::fs::symlink(dir.join("spec/index.pi"), dir.join("src/peek")).unwrap();
    let refused = prepare(ProfileKind::Code, Agent::Claude, &dir, &RunFiles::default())
        .err()
        .unwrap();
    assert!(
        refused.message.contains("links to the denied path"),
        "{}",
        refused.message
    );
    std::fs::remove_file(dir.join("src/peek")).unwrap();
    std::fs::hard_link(dir.join("spec/index.pi"), dir.join("src/peek")).unwrap();
    let refused = prepare(ProfileKind::Code, Agent::Claude, &dir, &RunFiles::default())
        .err()
        .unwrap();
    assert!(refused.message.contains("hard link"), "{}", refused.message);
    std::fs::remove_dir_all(&dir).ok();
}

/// A classification with a path both code and spec refuses every run.
#[test]
fn a_path_classed_as_both_refuses_runs() {
    let dir = project("both");
    Classification {
        code_roots: vec!["src".into()],
        spec_roots: vec!["spec".into(), "src".into()],
        ..Classification::default()
    }
    .save(&dir)
    .unwrap();
    assert!(prepare(ProfileKind::Code, Agent::Claude, &dir, &RunFiles::default()).is_err());
    assert!(prepare(ProfileKind::Spec, Agent::Claude, &dir, &RunFiles::default()).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

/// The Code profile reads and writes the code and reads the compiled
/// reference; the spec's source it can neither read, list, nor write, by
/// any route tried.
#[test]
fn the_code_profile_keeps_the_spec_out_of_reach() {
    if !enforced() {
        return;
    }
    let dir = project("code");
    let code = ProfileKind::Code;
    assert!(succeeds(
        code,
        &dir,
        "cat src/a.rs && echo more >> src/a.rs && echo new > src/b.rs"
    ));
    assert!(succeeds(code, &dir, "cat .claude/reference/index.md"));
    assert!(!succeeds(
        code,
        &dir,
        "echo x >> .claude/reference/index.md"
    ));
    for hostile in [
        "cat spec/index.pi",
        "ls spec",
        "echo x > spec/new.pi",
        "echo x >> spec/index.pi",
        "rm spec/index.pi",
        "cat src/../spec/index.pi",
        "cat $PWD/spec/index.pi",
        "cat /proc/self/cwd/spec/index.pi",
        // A child of a child.
        "sh -c 'sh -c \"cat spec/index.pi\"'",
        // Renamed across the boundary.
        "mv spec/index.pi src/stolen.pi",
        "mv src/a.rs spec/a.rs",
        // Hard linked from inside the run.
        "ln spec/index.pi src/h",
        // The project's own git, and its data.
        "cat .git/HEAD",
        "ls .suspense",
    ] {
        assert!(
            !succeeds(code, &dir, hostile),
            "`{hostile}` reached the spec"
        );
    }
    // Interpreters and compiled programs fare no better.
    if which("python3") {
        assert!(!succeeds(
            code,
            &dir,
            "python3 -c 'open(\"spec/index.pi\").read()'"
        ));
    }
    if which("node") {
        assert!(!succeeds(
            code,
            &dir,
            "node -e 'require(\"fs\").readFileSync(\"spec/index.pi\")'"
        ));
    }
    if which("cc") {
        let program = r#"cat > "$TMPDIR/peek.c" <<'C'
#include <stdio.h>
int main(void) { FILE *f = fopen("spec/index.pi", "r"); if (!f) return 1; int c; while ((c = fgetc(f)) != EOF) putchar(c); return 0; }
C
cc "$TMPDIR/peek.c" -o "$TMPDIR/peek" && "$TMPDIR/peek""#;
        let (code_, printed) = run(code, &dir, program);
        assert!(
            code_ != 0 && !printed.contains("spec source"),
            "a C program read the spec: {printed}"
        );
    }
    // A link made inside the run reaches nothing; once made, the next run
    // is refused before it starts.
    let (_, printed) = run(code, &dir, "ln -s ../spec/index.pi src/l; cat src/l");
    assert!(
        !printed.contains("spec source"),
        "a link read the spec: {printed}"
    );
    assert!(prepare(code, Agent::Claude, &dir, &RunFiles::default()).is_err());
    std::fs::remove_file(dir.join("src/l")).ok();
    // Nothing of the spec's source was changed or read.
    assert!(
        std::fs::read_to_string(dir.join("spec/index.pi"))
            .unwrap()
            .contains("secret")
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The Spec profile reads and writes the spec, and the code it can neither
/// read nor write.
#[test]
fn the_spec_profile_keeps_the_code_out_of_reach() {
    if !enforced() {
        return;
    }
    let dir = project("spec");
    let spec = ProfileKind::Spec;
    assert!(succeeds(
        spec,
        &dir,
        "cat spec/index.pi && echo 'export more: 2' > spec/more.pi"
    ));
    assert!(succeeds(spec, &dir, "echo x > .claude/reference/new.md"));
    for hostile in [
        "cat src/a.rs",
        "ls src",
        "echo x > src/b.rs",
        "cat spec/../src/a.rs",
        "mv src/a.rs spec/a.rs",
        "sh -c 'cat src/a.rs'",
    ] {
        assert!(
            !succeeds(spec, &dir, hostile),
            "`{hostile}` reached the code"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// Git, through the filtered repository, shows nothing denied: not at HEAD,
/// not in history, not in the stash, not among its objects.
#[test]
fn git_shows_nothing_denied() {
    if !enforced() {
        return;
    }
    let dir = project("git");
    let code = ProfileKind::Code;
    assert!(succeeds(
        code,
        &dir,
        "git status && git log --oneline && git show HEAD:src/a.rs"
    ));
    for hostile in [
        "git show HEAD:spec/index.pi",
        "git log -p --all | grep -q 'spec source'",
        "git stash show -p | grep -q spec",
        "git cat-file --batch-all-objects --batch | grep -q 'spec source'",
        "git --git-dir=.git show HEAD:spec/index.pi",
        "GIT_DIR=$PWD/.git git show HEAD:spec/index.pi",
    ] {
        assert!(
            !succeeds(code, &dir, hostile),
            "`{hostile}` showed the spec"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// A descriptor the application had open on a denied file is never
/// inherited by the run.
#[cfg(unix)]
#[test]
fn inherited_descriptors_are_closed() {
    if !enforced() {
        return;
    }
    let dir = project("fds");
    let path =
        std::ffi::CString::new(dir.join("spec/index.pi").to_string_lossy().as_bytes()).unwrap();
    // Opened without close-on-exec, as a leaked descriptor would be.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY) };
    assert!(fd >= 0);
    let (_, printed) = run(
        ProfileKind::Code,
        &dir,
        &format!("cat /proc/self/fd/{fd}; cat <&{fd}"),
    );
    unsafe { libc::close(fd) };
    assert!(
        !printed.contains("spec source"),
        "an inherited descriptor read the spec: {printed}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A path swapped for a link into a denied path between checking the
/// policy and granting it is refused, not granted.
#[cfg(unix)]
#[test]
fn a_path_swapped_after_the_check_is_refused() {
    if !enforced() {
        return;
    }
    let dir = project("race");
    let prepared = prepare(ProfileKind::Code, Agent::Claude, &dir, &RunFiles::default()).unwrap();
    std::fs::rename(dir.join("src"), dir.join("src-was")).unwrap();
    std::os::unix::fs::symlink(dir.join("spec"), dir.join("src")).unwrap();
    let mut command = Command::new("cat");
    command.arg("src/index.pi");
    match prepared.apply(&mut command) {
        Err(refused) => assert!(refused.message.contains("changed"), "{}", refused.message),
        Ok(()) => {
            let output = command.output().unwrap();
            assert!(!String::from_utf8_lossy(&output.stdout).contains("spec source"));
        }
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// A local service listening on a Unix socket outside the run's grants
/// can't be asked to read the project for it, where the kernel scopes them.
#[cfg(target_os = "linux")]
#[test]
fn unix_sockets_outside_the_grants_are_refused() {
    if !enforced()
        || !status().unwrap().contains("Unix sockets")
        || status().unwrap().contains("left out")
    {
        eprintln!("skipped: this kernel doesn't scope Unix sockets by path");
        return;
    }
    let dir = project("socket");
    let address =
        std::env::temp_dir().join(format!("suspense-service-{}.sock", std::process::id()));
    std::fs::remove_file(&address).ok();
    let listener = std::os::unix::net::UnixListener::bind(&address).unwrap();
    std::thread::spawn(move || {
        use std::io::Write as _;
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            stream.write_all(b"served spec source\n").ok();
        }
    });
    if which("python3") {
        let script = format!(
            "python3 -c 'import socket; s=socket.socket(socket.AF_UNIX); s.connect(\"{}\"); print(s.recv(100))'",
            address.display()
        );
        let (code, printed) = run(ProfileKind::Code, &dir, &script);
        assert!(
            code != 0 && !printed.contains("served"),
            "a socket outside the grants answered: {printed}"
        );
    }
    std::fs::remove_file(&address).ok();
    std::fs::remove_dir_all(&dir).ok();
}

/// A native build of the code still works in the Code profile.
#[test]
fn a_native_build_still_works() {
    if !enforced() || !which("cargo") {
        return;
    }
    let dir = project("build");
    std::fs::create_dir_all(dir.join("app/src")).unwrap();
    std::fs::write(
        dir.join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("app/src/main.rs"),
        "fn main() { println!(\"built\"); }\n",
    )
    .unwrap();
    Classification {
        code_roots: vec!["src".into(), "app".into()],
        spec_roots: vec!["spec".into()],
        ..Classification::default()
    }
    .save(&dir)
    .unwrap();
    let (code, printed) = run(
        ProfileKind::Code,
        &dir,
        "cd app && CARGO_TARGET_DIR=$PWD/target cargo run --offline -q",
    );
    assert!(
        code == 0 && printed.contains("built"),
        "the build failed: {printed}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The piton channel runs only what the run's profile allows.
#[test]
fn piton_is_only_run_as_the_profile_allows() {
    let args = |text: &str| text.split(' ').map(String::from).collect::<Vec<_>>();
    assert!(channel::allowed(ProfileKind::Spec, &args("build")).is_ok());
    assert!(channel::allowed(ProfileKind::Spec, &args("check")).is_ok());
    assert!(
        channel::allowed(
            ProfileKind::Code,
            &args("slice --adapter claude-code index.pi#A")
        )
        .is_ok()
    );
    assert!(channel::allowed(ProfileKind::Code, &args("slice index.pi#A")).is_err());
    assert!(channel::allowed(ProfileKind::Code, &args("build")).is_err());
    assert!(channel::allowed(ProfileKind::Question, &args("check")).is_err());
    assert!(channel::allowed(ProfileKind::Bare, &args("slice --adapter x y")).is_err());
}

fn which(program: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {program}")])
        .output()
        .is_ok_and(|output| output.status.success())
}
