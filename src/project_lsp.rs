//! The project's one `piton lsp`, shared by the chat input and the editor,
//! restarted whenever the project directory changes, and whenever it stops on
//! its own.

use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use gpui_kit::*;

use crate::piton_lsp::LspClient;
use crate::project_directory::ProjectDirectory;

/// How long after stopping on its own the server is started again.
const RESTART_DELAY: Duration = Duration::from_secs(1);

/// A server that stops sooner than this after starting failed quickly.
const QUICK_FAILURE: Duration = Duration::from_secs(10);

/// How often a running server is checked for having stopped.
const EXIT_POLL: Duration = Duration::from_millis(500);

/// Quick failures in a row after which the server is left stopped.
const MAX_QUICK_FAILURES: usize = 3;

#[derive(Default)]
pub struct ProjectLsp {
    /// The server, once it is running.
    client: Option<Arc<LspClient>>,
    /// The project the server was last started for, running or not.
    project: Option<PathBuf>,
}

impl Global for ProjectLsp {}

impl ProjectLsp {
    pub fn init(cx: &mut App) {
        cx.set_global(Self::default());
        cx.observe_global::<ProjectDirectory>(Self::restart)
            .detach();
        Self::restart(cx);
        // A project set in the same update as this, as the one opened last is
        // when the application starts, can come before the observer is in
        // place: catch up with it once the update is over.
        cx.defer(|cx| {
            if cx.global::<Self>().project != ProjectDirectory::get(cx) {
                Self::restart(cx);
            }
        });
    }

    /// The project's server, once it is running.
    pub fn get(cx: &App) -> Option<Arc<LspClient>> {
        cx.try_global::<Self>().and_then(|lsp| lsp.client.clone())
    }

    fn restart(cx: &mut App) {
        // Dropping the old server stops it.
        let project = ProjectDirectory::get(cx);
        cx.set_global(Self {
            client: None,
            project: project.clone(),
        });
        if let Some(project_dir) = project {
            Self::start(project_dir, 0, cx);
        }
    }

    /// Starts the server for `project_dir`, after `quick_failures` quick
    /// failures in a row, and starts it again should it stop on its own.
    fn start(project_dir: PathBuf, quick_failures: usize, cx: &mut App) {
        let start = cx.background_spawn({
            let project_dir = project_dir.clone();
            async move { LspClient::start(&project_dir) }
        });
        cx.spawn(async move |cx| {
            let client = match start.await {
                Ok(client) => Arc::new(client),
                Err(err) => {
                    eprintln!("piton lsp unavailable, language support disabled: {err:#}");
                    return;
                }
            };
            let started = Instant::now();
            // Only a weak hold while waiting, so replacing the server still
            // drops, and so stops, it.
            let watched = Arc::downgrade(&client);
            let kept = cx.update(|cx| {
                // A server for a project left behind while it started is
                // dropped.
                let current = ProjectDirectory::get(cx).as_ref() == Some(&project_dir);
                if current {
                    cx.global_mut::<Self>().client = Some(client);
                }
                current
            });
            if !kept {
                return;
            }
            loop {
                cx.background_executor().timer(EXIT_POLL).await;
                match watched.upgrade() {
                    Some(client) if client.has_exited() => break,
                    Some(_) => {}
                    // Replaced, and so stopped on purpose.
                    None => return,
                }
            }

            let quick_failures = if started.elapsed() < QUICK_FAILURE {
                quick_failures + 1
            } else {
                0
            };
            let stopped_on_its_own = cx.update(|cx| {
                let current = Self::get(cx)
                    .is_some_and(|client| Weak::as_ptr(&watched) == Arc::as_ptr(&client));
                if current {
                    if let Some(client) = Self::get(cx) {
                        eprintln!("piton lsp stopped: {}", client.exit_report());
                    }
                    cx.global_mut::<Self>().client = None;
                }
                current
            });
            if !stopped_on_its_own {
                return;
            }
            if quick_failures >= MAX_QUICK_FAILURES {
                eprintln!(
                    "piton lsp stopped {quick_failures} times in a row soon after starting; \
                     language support disabled until the project is opened again"
                );
                return;
            }
            cx.background_executor().timer(RESTART_DELAY).await;
            cx.update(|cx| {
                // Unless the project changed meanwhile, which starts its own.
                if Self::get(cx).is_none()
                    && ProjectDirectory::get(cx).as_ref() == Some(&project_dir)
                {
                    Self::start(project_dir, quick_failures, cx);
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use gpui_kit::TestAppContext;

    use super::ProjectLsp;
    use crate::piton_lsp::{LspClient, PitonSession};
    use crate::project_directory::ProjectDirectory;

    /// Runs the app until `done`, letting the server's own threads and the
    /// restart delay's timer move on too.
    fn run_until(cx: &mut TestAppContext, mut done: impl FnMut(&mut TestAppContext) -> bool) {
        for _ in 0..200 {
            if done(cx) {
                return;
            }
            cx.executor().advance_clock(Duration::from_millis(250));
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out");
    }

    fn server(cx: &mut TestAppContext) -> Option<Arc<LspClient>> {
        cx.update(|cx| ProjectLsp::get(cx))
    }

    /// As the application starts: the project opened last is set straight
    /// after the server is set up, in the same update, and still gets a
    /// server.
    #[gpui_kit::test]
    async fn a_project_set_as_the_application_starts_gets_a_server(cx: &mut TestAppContext) {
        if crate::piton_build::piton_missing() {
            return;
        }
        cx.update(|cx| {
            ProjectDirectory::init(cx);
            ProjectLsp::init(cx);
            ProjectDirectory::set(PathBuf::from(env!("CARGO_MANIFEST_DIR")), cx);
        });
        run_until(cx, |cx| server(cx).is_some());
    }

    /// A server that stops on its own is replaced by a new one that
    /// completes; one that keeps stopping straight after starting is left
    /// stopped.
    #[gpui_kit::test]
    async fn a_stopped_server_restarts_until_it_keeps_failing(cx: &mut TestAppContext) {
        if crate::piton_build::piton_missing() {
            return;
        }
        cx.update(|cx| {
            ProjectDirectory::init(cx);
            ProjectDirectory::set(PathBuf::from(env!("CARGO_MANIFEST_DIR")), cx);
            ProjectLsp::init(cx);
        });
        run_until(cx, |cx| server(cx).is_some());

        for failure in 1..=3 {
            let crashed = server(cx).unwrap();
            crashed.kill();
            run_until(cx, |cx| server(cx).is_none());
            if failure < 3 {
                run_until(cx, |cx| {
                    server(cx).is_some_and(|server| !Arc::ptr_eq(&server, &crashed))
                });
                let session = PitonSession::new(server(cx).unwrap());
                let prompt = "Update @{Appl";
                let completions =
                    serde_json::to_string(&session.complete(prompt, prompt.len()).unwrap())
                        .unwrap();
                assert!(completions.contains("ApplicationScope"), "{completions}");
            }
        }

        // The third quick failure in a row leaves it stopped.
        for _ in 0..20 {
            cx.executor().advance_clock(Duration::from_millis(500));
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(server(cx).is_none());

        // Opening the project again starts it afresh.
        cx.update(|cx| ProjectDirectory::set(PathBuf::from(env!("CARGO_MANIFEST_DIR")), cx));
        run_until(cx, |cx| server(cx).is_some());
    }
}
