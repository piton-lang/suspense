//! The project's one `piton lsp`, shared by the chat input and the editor,
//! and restarted whenever the project directory changes.

use std::sync::Arc;

use gpui_kit::*;

use crate::piton_lsp::LspClient;
use crate::project_directory::ProjectDirectory;

#[derive(Default)]
pub struct ProjectLsp(Option<Arc<LspClient>>);

impl Global for ProjectLsp {}

impl ProjectLsp {
    pub fn init(cx: &mut App) {
        cx.set_global(Self::default());
        cx.observe_global::<ProjectDirectory>(Self::restart)
            .detach();
        Self::restart(cx);
    }

    /// The project's server, once it is running.
    pub fn get(cx: &App) -> Option<Arc<LspClient>> {
        cx.try_global::<Self>().and_then(|lsp| lsp.0.clone())
    }

    fn restart(cx: &mut App) {
        // Dropping the old server stops it.
        cx.set_global(Self(None));
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };

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
            cx.update(|cx| {
                // A server for a project left behind while it started is
                // dropped.
                if ProjectDirectory::get(cx).as_ref() == Some(&project_dir) {
                    cx.set_global(Self(Some(client)));
                }
            });
        })
        .detach();
    }
}
