# Suspense's sandbox

Every harness run Suspense starts — Claude Code, Codex, or OpenCode, and everything that run starts in turn — runs natively on the host under a filesystem policy the operating system enforces. A **Code** run can't read or change the spec's source. A **Spec** run can't read or change the code. There is no container, VM, or WSL. The module is `src/sandbox/`.

## Guarantees

| | Linux (Landlock) | macOS (Seatbelt) | Windows (AppContainer) |
|---|---|---|---|
| Status | **Enforced and tested** | Implemented, **untested** | Implemented, **untested**: type-checked only |
| Read / write / list / execute denied outside grants | yes (ABI ≥ 3) | yes, by profile | yes, by access entries for the run's AppContainer SID |
| Truncate | yes (ABI ≥ 3) | yes | yes (a write) |
| Rename / link across the boundary | yes (`Refer`, ABI ≥ 2) | yes | yes: both ends need access |
| Inherited by every descendant, can't be lifted | yes (`no_new_privs`, Landlock domain) | yes | yes: AppContainer token, job with kill-on-close and no breakaway |
| Abstract Unix sockets and signals scoped to the run | ABI ≥ 6 | — | processes outside the AppContainer can't be opened |
| Connecting to path-named Unix sockets outside grants | ABI ≥ 9 (`ResolveUnix`, Linux 7.1) | **no**: the profile allows `network*`, which includes Unix sockets | named pipes and services: only those whose access lists name the AppContainer |
| Inherited file descriptors closed | yes (`close_range(CLOEXEC)`) | inherits the parent's close-on-exec only | yes: `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` names only stdin/stdout/stderr |
| Fails closed | yes: no Landlock, or ABI < 3, means no run starts | yes, if `/usr/bin/sandbox-exec` is missing | yes: older than 1809 (build 17763), or any AppContainer, capability, grant or ledger step failing, means no run starts |

**On Linux** the ruleset is built in Suspense, with every granted path opened as a descriptor and checked against the identity it had when the policy was compiled. It is applied in the child just before `exec`, with `no_new_privs` set first. Nothing is applied to Suspense itself, and no root is needed. Landlock has no notion of "deny inside an allow". So the policy is compiled to an allow-list: a granted folder that holds a denied path is expanded into grants of its other entries, level by level.

**On macOS** a Seatbelt profile (`(deny default)` + explicit `file-read*`/`file-write*` subpaths + trailing `deny` rules for denied paths) is generated per run and the harness is started through `/usr/bin/sandbox-exec -p`. Apple has deprecated `sandbox-exec`, and its profile language is undocumented. It still works on current macOS and is what Codex, Chrome and agent-safehouse rely on. This backend **has not been run on a Mac yet**: until it passes `cargo test sandbox::` there, treat macOS as unprotected.

**On Windows** each run gets an AppContainer of its own, `Suspense.<profile>.<pid>-<n>`, made with `CreateAppContainerProfile` and deleted with `DeleteAppContainerProfile` after it. The harness is started through Suspense's own program, `suspense --sandbox-launch <spec> -- <program>…`, the way macOS starts it through `sandbox-exec`. The launcher, which isn't contained, does the rest:

1. It gives each granted path an access entry for the AppContainer's SID alone: read and execute, or modify. The entry is inherited by everything beneath, and is set with `SetNamedSecurityInfoW`. A path that is a reparse point (a link or junction) is never granted. Each entry is written to a ledger in `%LOCALAPPDATA%\suspense\sandbox-ledger` *before* it is added. The ledger is replayed, and every entry removed, after the run, when the run's sandbox is dropped, and at the next launch (after a crash).
2. It starts the harness with `CreateProcessW`: suspended, with no console window, given only the `internetClient` capability, and inheriting only its three standard handles. The harness is put in a job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (breakaway isn't allowed) and only then resumed. A `.cmd`/`.bat` harness, as npm installs, runs through `cmd /d /c`.
3. It waits, then removes the grants, the AppContainer and the ledger.

The piton channel is a named pipe, `\\.\pipe\suspense-<run>`. Its access list names only the run's AppContainer SID and the user. Its first instance is made with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so nothing can squat it. The shim is `bin\piton.cmd`.

Read-write grants are also labelled **low integrity** (`ML;OICI;NW;;;LW`) for the run, because an AppContainer runs at low integrity, and Windows' no-write-up rule would otherwise stop it writing even with an access entry. This goes beyond what the spec describes. While the run lasts, another low-integrity process of the same user could write there too. The label is recorded in the ledger and removed with the grants.

`C:\Windows` and `C:\Program Files` are readable by every AppContainer through ALL APPLICATION PACKAGES, and are given nothing. Nothing is ever granted to that group. Node, git, PowerShell, MSVC's `cl`/`link`, Cargo and the harness are granted read-only by the folder their program resolves to. `.cargo`, `.rustup` and the PowerShell module folders are granted read-only by path. The npm, pip, node-gyp and Visual Studio caches, the harness's own profile folder and the run's temp folder are granted read-write.

This backend **has not run on Windows yet**. It type-checks against `x86_64-pc-windows-msvc`. Its Windows tests (`cargo test sandbox::windows`) cover reaching a granted file but not a denied one, junctions, symbolic links, hard links, 8.3 short names, alternate data streams, opening a process outside the job, and no access entry remaining after a run. Until they pass on a Windows runner, treat Windows as unprotected.

## Profiles

Built by the application from the run's kind, never chosen by the run. The full rules are in the Sandbox spec's profiles section.

- **Code**: the code roots and build output (`target`, `node_modules`, `build`, `dist`) read-write; the harness directory (`.claude` etc., holding the compiled reference) read-only; the spec roots denied.
- **Spec**: the spec roots, the harness directory and `.piton` read-write; `piton.config.pi` read-only; the code roots denied unless the project lets Spec runs read them, when they're read-only.
- **Question**: the code, the spec, the harness directory and the config, all read-only.
- **Freeform**: the whole project, with the real `.git`; `.suspense` excluded.
- **Bare** (commit notes, titles, commit messages): nothing in the project.

Every profile also gets:

- its attached files, read-only;
- its understanding file, read-write;
- the two fluency files, read-only;
- the project's top-level `CLAUDE.md`, `AGENTS.md` and `.mcp.json`, read-only;
- a temporary directory of its own (`TMPDIR`/`TMP`/`TEMP`), removed after the run;
- read-only system and toolchain folders (`/usr`, `/etc`, `/opt`, `/proc`, `/run`, `~/.cargo`, `~/.rustup`, `~/.local`, `~/.nvm`, …) and the harness's installation;
- read-write harness folders (`~/.claude`, `~/.claude.json`, `~/.codex`, OpenCode's) and caches (`~/.cache`, `~/.npm`, `~/.cargo/registry`, `~/.cargo/git`).

A grant outside the project that would contain the project, such as a project under `/opt`, is cut at the project's edge.

The classification of paths as code or spec lives in `.suspense/sandbox.json` (code roots, spec roots, glob patterns). It is edited in **Settings → Sandbox**. Without the file, the project's `codeRoot` and `root` are used. A path classed as both blocks every run. A path classed as neither is denied to Code and Spec runs alike. So with `codeRoot: ./src`, a Code run can't read `Cargo.toml` until it is added as a code root.

## Escape routes covered

These are tested by `cargo test sandbox::`, and by `harness::tests::a_task_runs_under_its_sandbox` through a real `send_task`. Each test runs real commands under each profile in a temporary project:

- reading, listing, writing, appending and deleting denied files with a shell;
- the same through Python, Node.js, and a C program compiled with `cc` inside the sandbox;
- a child of a child (`sh -c 'sh -c …'`);
- `..` paths, `$PWD`-absolute paths, `/proc/self/cwd/...`;
- symbolic links into a denied path, made before the run (refused at compile time) or inside it (refused at access time, then refused at the next compile);
- hard links to a denied file, made before the run (found by device and inode, refused) or inside it (`EXDEV`);
- renames across the boundary in both directions;
- a descriptor on a denied file left open without close-on-exec in the parent;
- a path swapped for a link into a denied folder between compiling and applying (refused: identity changed);
- the project's `.git` and `.suspense`;
- git:
  - restricted runs get `GIT_DIR` pointing at a filtered repository with one commit and no history, holding only blobs the profile may read, copied with `pack-objects` from the permitted SHAs;
  - `git show HEAD:spec/…`, `git log -p --all`, `git stash show`, `git cat-file --batch-all-objects`, `--git-dir=.git` and `GIT_DIR=.git` all show nothing denied;
- a local service on a Unix socket outside the grants (ABI ≥ 9);
- `piton`:
  - the run's `piton` is a shim that asks Suspense over a per-run Unix socket in the run's temporary directory;
  - Spec runs may `build`/`check`/`fmt`/`slice`; Code and Question runs may only `slice --adapter …`, which prints compiled output;
  - anything else is refused;
- a contradictory classification refuses the run before anything starts;
- `cargo run --offline` of a crate in the code still works.

The tests skip with a message, rather than fail, where the platform's backend can't run.

## What remains outside the boundary

These are not claimed as protected:

- **The network.** A run can fetch the project from wherever it's published: a remote, a forge, a CI artifact.
- **The harness's own service.** It holds earlier conversations of the same lane, which may include spec text sent in an earlier Spec task.
- **The clipboard.**
- **Path-named Unix sockets on Linux kernels before 7.1** (Landlock ABI < 9). The user's D-Bus session bus can't be blocked there, and through it, for example `systemd-run --user`, a run could start an unrestricted process. **This is a real bypass on older kernels.** Settings shows the ABI and what it covers.
- **Names, not contents.** `stat` isn't restricted by Landlock, so a run can learn that `spec/index.pi` exists and its size. It can't read or list it.
- **The project root can't be listed.** Granting `ReadDir` on it would also grant listing the denied folders beneath it, so the root is never granted. Tools that walk from the root (`git status` untracked files, `rg` from `.`) must start from a granted folder. The filtered repository sets `status.showUntrackedFiles=no` for this reason.
- **Files the harness itself writes outside its folders**, such as a lock file beside `~/.claude.json`, fail if they fall outside the grants.

## Choice of approach

| Project | Licence | What it enforces | Why not reused |
|---|---|---|---|
| [nolabs-ai/nono](https://github.com/nolabs-ai/nono) | Apache-2.0 | Agent sandbox (Rust); no daemon/container; Windows only through WSL2 | Pre-1.0, APIs still changing; a full agent-runtime product rather than a library for a per-command filesystem policy; Windows via WSL2 is excluded by the requirements |
| [openai/codex](https://github.com/openai/codex) sandboxes | Apache-2.0 | Linux: Landlock + seccomp (optional vendored bubblewrap); macOS: Seatbelt; Windows: restricted tokens, ACLs, sandbox user | Closest in shape. It's built into the Codex CLI, though, not published as a standalone crate. Its policies are workspace-write/read-only, not allow/deny sets with spec/code separation. The approach (Landlock on Linux, generated Seatbelt profile on macOS) was followed. |
| [anthropics/sandbox-runtime](https://github.com/anthropic-experimental/sandbox-runtime) | Apache-2.0 | Linux: bubblewrap + seccomp; macOS: Seatbelt; Windows (alpha): sandbox user + NTFS ACEs + WFP | TypeScript plus native helpers; needs `bwrap` and `socat`; reads are allowed by default with deny lists, and a skipped `denyRead` glob enforces nothing on Linux — the opposite default to the one needed here |
| [eugene1g/agent-safehouse](https://github.com/eugene1g/agent-safehouse) | Apache-2.0 | macOS only, `sandbox-exec` profiles | macOS only; a shell script; describes itself as a hardening layer rather than a boundary. Its deny-first Seatbelt profile style informed the macOS backend. |
| [LeGambiArt/arapuca](https://github.com/LeGambiArt/arapuca) | Apache-2.0 | Linux: Landlock + seccomp + namespaces + cgroups; macOS: Seatbelt; Windows: AppContainer + job objects | Young (no releases, small community); much more than a filesystem policy (cgroups, micro-VMs, network namespaces), which would be a large framework to bring in. Its Windows design (AppContainer + job object) is the one the spec chose for Suspense's Windows backend. |

**Chosen:** the [`landlock`](https://crates.io/crates/landlock) crate (MIT/Apache-2.0, maintained by Landlock's author) for Linux, used directly. On macOS, a profile generated for `sandbox-exec`. No other new dependencies, apart from `globset` for classification patterns. Each backend is small enough to audit in one sitting.

## Examples

`suspense --sandbox <profile> <project> -- <command>…` runs one command under a profile, as a run would:

```sh
# Code profile: the code is readable, the spec is not
suspense --sandbox code ~/my-project -- sh -c 'cat src/main.rs; cat spec/index.pi'

# Spec profile: the other way round, and piton through the controlled channel
suspense --sandbox spec ~/my-project -- sh -c 'cat spec/index.pi; cat src/main.rs; piton check'

# git sees only what the profile may read
suspense --sandbox code ~/my-project -- git show HEAD:spec/index.pi
```

It prints the profile's grant counts, then the command's own output, and exits with the command's status. If the sandbox can't be applied, it prints why and exits with status 1.
