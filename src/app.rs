//! Application bootstrap: starts the platform, initializes GPUI Kit and opens
//! the main window.

use gpui_kit::*;

use crate::file_view;
use crate::main_window::{self, MainWindow};
use crate::piton_syntax;
use crate::project_directory::ProjectDirectory;
use crate::project_lsp::ProjectLsp;

pub const APP_TITLE: &str = "Suspense";

actions!(suspense, [Quit]);

pub fn run() {
    // The full icon catalogue: the default bundle leaves out icons the app
    // uses, such as the Edit and Reply badges', which then render blank.
    let app = gpui_kit::application().with_assets(gpui_kit::assets::AllAssets);

    app.run(|cx| {
        gpui_kit::init(cx);
        piton_syntax::init();
        ProjectDirectory::init(cx);
        ProjectLsp::init(cx);

        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            #[cfg(target_os = "macos")]
            KeyBinding::new("cmd-q", Quit, None),
            #[cfg(not(target_os = "macos"))]
            KeyBinding::new("ctrl-q", Quit, None),
        ]);
        main_window::bind_keys(cx);
        file_view::bind_keys(cx);

        // Closing the last window ends the application on every platform.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        MainWindow::open(cx).expect("failed to open main window");
        cx.activate(true);
    });
}
