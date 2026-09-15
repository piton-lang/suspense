//! The main window of the application, from which all main functionality is
//! reached: a toolbar on top, and below it the project tree in a sidebar on the
//! left, then prompt mode.

use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::{ActiveTheme, Root, WindowExt as _};
use gpui_kit::*;

use crate::app::{APP_TITLE, Quit};
use crate::palette::{Palette, Picked, SystemCommand, SystemState};
use crate::project_tree::{OpenFile, ProjectTree};
use crate::prompt_mode::PromptMode;
use crate::theme_preference;
use crate::toolbar::{self, Toolbar};

/// Size the window restores to when it is un-maximized.
const RESTORE_SIZE: Size<Pixels> = size(px(1280.), px(800.));

/// The narrowest a split can be dragged.
const MIN_SPLIT_WIDTH: Pixels = px(240.);

/// The sidebar's width until it is dragged, and the narrowest it can be.
const SIDEBAR_WIDTH: Pixels = px(260.);
const MIN_SIDEBAR_WIDTH: Pixels = px(160.);

actions!(suspense, [FocusChat, TogglePalette]);

/// Esc anywhere in the window moves focus to the chat input. It is bound
/// without a context, so an editor or popup that has something to cancel
/// (a completion menu, extra cursors) takes Esc first. Ctrl/Cmd+P opens or
/// closes the palette.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", FocusChat, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-p", TogglePalette, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-p", TogglePalette, None),
    ]);
    crate::palette::bind_keys(cx);
}

pub struct MainWindow {
    toolbar: Entity<Toolbar>,
    sidebar: Entity<ProjectTree>,
    prompt_mode: Entity<PromptMode>,
    /// The palette last opened, which may since have closed.
    palette: Option<Entity<Palette>>,
    _palette_subscription: Option<Subscription>,
    sidebar_split: Entity<ResizableState>,
    _subscriptions: Vec<Subscription>,
}

impl MainWindow {
    pub fn open(cx: &mut App) -> Result<WindowHandle<Root>> {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Maximized(Bounds::centered(
                None,
                RESTORE_SIZE,
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some(APP_TITLE.into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        cx.open_window(options, |window, cx| {
            // Start in the saved light or dark mode, else the system's; the
            // toolbar can switch.
            theme_preference::apply(window, cx);
            let view = cx.new(|cx| MainWindow::new(window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
    }

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let toolbar = cx.new(Toolbar::new);
        let sidebar = cx.new(ProjectTree::new);
        let subscriptions = vec![
            cx.subscribe_in(&sidebar, window, |this, _, OpenFile(path), window, cx| {
                this.prompt_mode.update(cx, |prompt_mode, cx| {
                    prompt_mode.open_file(path.clone(), window, cx)
                })
            }),
            // Until light or dark mode is chosen, the window keeps following
            // the system's appearance as it changes.
            cx.observe_window_appearance(window, |_, window, cx| {
                theme_preference::apply(window, cx)
            }),
            // Focus left on nothing, as when a focused file is closed, would
            // put Esc out of the window's reach; the chat input takes it.
            cx.on_focus_lost(window, |this, window, cx| {
                this.prompt_mode
                    .update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx))
            }),
        ];
        // Closing the window ends the application, so it asks first too.
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| this.confirm_quit(window, cx))
                .unwrap_or(true)
        });

        Self {
            toolbar,
            sidebar,
            prompt_mode: cx.new(|cx| PromptMode::new(window, cx)),
            palette: None,
            _palette_subscription: None,
            sidebar_split: cx.new(|_| ResizableState::default()),
            _subscriptions: subscriptions,
        }
    }

    /// Quits, once confirmed if a task is running.
    fn quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_quit(window, cx) {
            cx.quit();
        }
    }

    /// Whether the application can end now: nothing is running. While a
    /// prompt or a build is, it asks instead, quitting once confirmed.
    fn confirm_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let working = self.prompt_mode.read(cx).is_working();
        let building = self.toolbar.read(cx).is_building();
        let description = match (working, building) {
            (false, false) => return true,
            (true, false) => "The harness is still working on a prompt.",
            (false, true) => "piton build is still running.",
            (true, true) => "The harness is still working on a prompt and piton build is still running.",
        };
        // An open palette makes way; a confirmation already open stays the
        // only one.
        if let Some(palette) = &self.palette
            && palette.read(cx).is_open(window, cx)
        {
            window.close_dialog(cx);
        }
        if !window.has_active_dialog(cx) {
            window.open_alert_dialog(cx, move |alert, _, _| {
                alert
                    .title("Quit while a task is running?")
                    .description(description)
                    .button_props(
                        DialogButtonProps::default()
                            .show_cancel(true)
                            .ok_text("Quit")
                            .ok_variant(ButtonVariant::Danger)
                            .cancel_text("Keep Running"),
                    )
                    .on_ok(|_, _, cx| {
                        cx.quit();
                        true
                    })
            });
        }
        false
    }

    /// Opens a fresh palette on the Files tab, or closes the one open.
    fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(palette) = &self.palette
            && palette.read(cx).is_open(window, cx)
        {
            window.close_dialog(cx);
            return;
        }
        let toolbar = self.toolbar.read(cx);
        let system = SystemState {
            can_build: toolbar.can_build(cx),
            dark: cx.theme().is_dark(),
        };
        let palette = cx.new(|cx| Palette::new(system, window, cx));
        self._palette_subscription = Some(cx.subscribe_in(
            &palette,
            window,
            |this, _, picked: &Picked, window, cx| this.run_picked(picked, window, cx),
        ));
        Palette::open(&palette, window, cx);
        self.palette = Some(palette);
    }

    fn run_picked(&mut self, picked: &Picked, window: &mut Window, cx: &mut Context<Self>) {
        match picked {
            Picked::File(path) => self.prompt_mode.update(cx, |prompt_mode, cx| {
                prompt_mode.open_file(path.clone(), window, cx)
            }),
            Picked::Mention(mention) => self.prompt_mode.update(cx, |prompt_mode, cx| {
                prompt_mode.insert_mention(mention, window, cx)
            }),
            Picked::System(command) => match command {
                SystemCommand::OpenProject => self
                    .toolbar
                    .update(cx, |toolbar, cx| toolbar.pick_project(window, cx)),
                SystemCommand::Build => self
                    .toolbar
                    .update(cx, |toolbar, cx| toolbar.build(window, cx)),
                SystemCommand::ToggleDarkMode => {
                    toolbar::set_dark_mode(!cx.theme().is_dark(), window, cx)
                }
                SystemCommand::Quit => self.quit(window, cx),
            },
        }
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &FocusChat, window, cx| {
                // The context-less Esc binding outranks the palette's own, so
                // the palette's Esc is handled here.
                if let Some(palette) = &this.palette
                    && palette.read(cx).is_open(window, cx)
                {
                    palette.update(cx, |palette, cx| palette.cancel(window, cx));
                    return;
                }
                // Likewise any other dialog, such as the quit confirmation.
                if window.has_active_dialog(cx) {
                    window.close_dialog(cx);
                    return;
                }
                this.prompt_mode
                    .update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx))
            }))
            .on_action(
                cx.listener(|this, _: &TogglePalette, window, cx| this.toggle_palette(window, cx)),
            )
            .on_action(cx.listener(|this, _: &Quit, window, cx| this.quit(window, cx)))
            .child(self.toolbar.clone())
            .child(
                div().flex_1().min_h_0().child(
                    // One `children` call for every panel: the group's
                    // `children` replaces panels added before it.
                    h_resizable("sidebar-split")
                        .with_state(&self.sidebar_split)
                        .children([
                            resizable_panel()
                                .size(SIDEBAR_WIDTH)
                                .size_range(MIN_SIDEBAR_WIDTH..Pixels::MAX)
                                .child(self.sidebar.clone()),
                            resizable_panel()
                                .size_range(MIN_SPLIT_WIDTH..Pixels::MAX)
                                .child(self.prompt_mode.clone()),
                        ]),
                ),
            )
            // Inside the window's element tree, so actions such as
            // TogglePalette reach it from a focused dialog.
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: globbing `gpui_kit::*` would bring in GPUI's `test`
    // macro and shadow Rust's `#[test]`.
    use std::time::Duration;

    use gpui_kit::component::Root;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use gpui_kit::{AppContext as _, TestAppContext};

    use super::MainWindow;
    use crate::piton_syntax;
    use crate::project_directory::ProjectDirectory;

    const TIMEOUT: Duration = Duration::from_secs(2);

    /// The chat input has focus as soon as the window opens: a prompt typed
    /// and sent without clicking anywhere is sent, which without a project
    /// open shows a notification saying so.
    #[gpui_kit::test]
    async fn chat_input_is_focused_when_the_window_opens(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            Root::new(view, window, cx)
        });
        let handle = window.into();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;

        for key in "hello".chars() {
            cx.update_window(handle, |_, window, cx| window.input(&key.to_string(), cx))
                .unwrap();
            cx.run_until_parked();
        }
        #[cfg(target_os = "macos")]
        let send = "cmd-enter";
        #[cfg(not(target_os = "macos"))]
        let send = "ctrl-enter";
        cx.update_window(handle, |_, window, cx| window.press(send, cx))
            .unwrap();

        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("notification").is_some()
        })
        .await;
    }

    /// Clicking a file in the project tree opens it beside the chat history,
    /// taking 50% of the width; the chat input stays unsplit below both.
    #[gpui_kit::test]
    async fn clicking_a_file_opens_it_beside_the_chat_history(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-open-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "# Notes\n").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            Root::new(view, window, cx)
        });
        let handle = window.into();

        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find(("project-entry", 0usize)).is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.click(("project-entry", 0usize), cx)
        })
        .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("file-view").is_some()
        })
        .await;
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
        }

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let file = window.find("file-view").bounds();
            let history = window.find("history").bounds();
            let editor = window.find("prompt-editor").bounds();
            let send = window.find("send").bounds();
            let share = file.size.width / (file.size.width + history.size.width);
            assert!(
                (share - 0.5).abs() < 0.02,
                "file takes {share} of the width: {file:?} beside {history:?}"
            );
            assert!(
                history.left() >= file.right() - gpui_kit::px(1.),
                "history is not right of the file: {file:?} then {history:?}"
            );
            assert!(
                editor.top() >= file.bottom()
                    && editor.left() < file.right()
                    && send.right() > history.left(),
                "chat input (editor {editor:?}, Send {send:?}) is split with the file {file:?}"
            );
        })
        .unwrap();

        cx.update_window(handle, |_, window, cx| window.click("close-file", cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("file-view").is_none()
        })
        .await;

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Esc in an open file moves focus back to the chat input, where typing
    /// then lands.
    #[gpui_kit::test]
    async fn escape_in_a_file_focuses_the_chat_input(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-escape-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "# Notes\n").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();

        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find(("project-entry", 0usize)).is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.click(("project-entry", 0usize), cx)
        })
        .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("file-view").is_some()
        })
        .await;

        let file = main.read_with(cx, |main, cx| {
            main.prompt_mode.read(cx).open_file_view().unwrap()
        });
        let chat = main.read_with(cx, |main, cx| main.prompt_mode.read(cx).chat_input_view());
        cx.update_window(handle, |_, window, cx| {
            file.update(cx, |file, cx| file.focus_editor(window, cx));
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| window.input("hi", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| assert_eq!(chat.read(cx).value(cx).as_ref(), "hi"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Esc in the chat input takes focus out of it, and the window's own Esc
    /// does not hand focus straight back: typing no longer lands in it.
    #[gpui_kit::test]
    async fn escape_in_the_chat_input_takes_focus_out(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;

        let chat = main.read_with(cx, |main, cx| main.prompt_mode.read(cx).chat_input_view());
        cx.update_window(handle, |_, window, cx| {
            assert!(chat.read(cx).is_focused(window, cx));
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| {
            assert!(
                !chat.read(cx).is_focused(window, cx),
                "Esc left focus in the chat input"
            );
            window.input("hi", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| assert_eq!(chat.read(cx).value(cx).as_ref(), ""));
    }

    /// The project tree sits in a sidebar along the left, before prompt mode.
    #[gpui_kit::test]
    async fn sidebar_is_left_of_prompt_mode(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            Root::new(view, window, cx)
        });
        let handle = window.into();

        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("project-tree").is_some() && window.try_find("send").is_some()
        })
        .await;
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
        }

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let sidebar = window.find("project-tree").bounds();
            let send = window.find("send").bounds();
            assert!(
                sidebar.left() <= gpui_kit::px(1.) && sidebar.size.width > gpui_kit::px(0.),
                "sidebar is not along the left: {sidebar:?}"
            );
            assert!(
                send.left() >= sidebar.right(),
                "prompt mode (Send at {send:?}) overlaps the sidebar: {sidebar:?}"
            );
        })
        .unwrap();
    }

    /// While a prompt runs, quitting or closing the window asks first: Keep
    /// Running or Esc dismisses the question and leaves the window open. With
    /// nothing running the window closes straight away.
    #[gpui_kit::test]
    async fn quitting_while_a_task_runs_asks_first(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;
        main.update(cx, |main, cx| {
            main.prompt_mode
                .update(cx, |prompt_mode, _| prompt_mode.set_working(true))
        });

        // Asked twice, it still asks once: one Keep Running dismisses it.
        for _ in 0..2 {
            cx.update_window(handle, |_, window, cx| {
                window.dispatch_action(Box::new(crate::app::Quit), cx)
            })
            .unwrap();
            cx.run_until_parked();
        }
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("cancel").is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| window.click("cancel", cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("cancel").is_none()
        })
        .await;

        assert!(
            !gpui_kit::VisualTestContext::from_window(handle, cx).simulate_close(),
            "the window closed while a prompt was running"
        );
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("cancel").is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| window.press("escape", cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("cancel").is_none()
        })
        .await;

        main.update(cx, |main, cx| {
            main.prompt_mode
                .update(cx, |prompt_mode, _| prompt_mode.set_working(false))
        });
        assert!(gpui_kit::VisualTestContext::from_window(handle, cx).simulate_close());
    }

    #[cfg(target_os = "macos")]
    const PALETTE: &str = "cmd-p";
    #[cfg(not(target_os = "macos"))]
    const PALETTE: &str = "ctrl-p";

    /// Ctrl/Cmd+P opens the palette on the Files tab, where typing narrows the
    /// project's files and Enter opens the highlighted one. Opened again,
    /// Shift+Tab wraps round to Agents, where Enter inserts the agent into
    /// the chat input.
    #[gpui_kit::test]
    async fn palette_opens_files_and_inserts_agents(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-palette-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("notes.md"), "# Notes\n").unwrap();
        std::fs::write(dir.join("src/lib.rs"), "").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;

        cx.update_window(handle, |_, window, cx| window.press(PALETTE, cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("palette").is_some()
        })
        .await;
        let palette = main.read_with(cx, |main, _| main.palette.clone().unwrap());
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            palette.read(cx).result_labels() == ["notes.md", "src/lib.rs"]
        })
        .await;
        for key in "notes".chars() {
            cx.update_window(handle, |_, window, cx| window.input(&key.to_string(), cx))
                .unwrap();
            cx.run_until_parked();
        }
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            palette.read(cx).result_labels() == ["notes.md"]
        })
        .await;
        cx.update_window(handle, |_, window, cx| window.press("enter", cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, cx| {
            window.try_find("file-view").is_some() && !palette.read(cx).is_open(window, cx)
        })
        .await;

        cx.update_window(handle, |_, window, cx| window.press(PALETTE, cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            main.read(cx)
                .palette
                .as_ref()
                .is_some_and(|open| open != &palette)
        })
        .await;
        let palette = main.read_with(cx, |main, _| main.palette.clone().unwrap());
        cx.update_window(handle, |_, window, cx| window.press("shift-tab", cx))
            .unwrap();
        for key in "Explore".chars() {
            cx.update_window(handle, |_, window, cx| window.input(&key.to_string(), cx))
                .unwrap();
            cx.run_until_parked();
        }
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            palette
                .read(cx)
                .result_labels()
                .first()
                .is_some_and(|label| label == "Explore")
        })
        .await;
        cx.update_window(handle, |_, window, cx| window.press("enter", cx))
            .unwrap();

        let chat = main.read_with(cx, |main, cx| main.prompt_mode.read(cx).chat_input_view());
        cx.wait_for(handle, TIMEOUT, |window, cx| {
            chat.read(cx).value(cx).as_ref() == "@agent-Explore "
                && chat.read(cx).is_focused(window, cx)
        })
        .await;

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A harness that finds `notes.md`.
    fn finds_notes(
        _: String,
        system_prompt: Option<String>,
        _: std::path::PathBuf,
    ) -> futures::channel::mpsc::UnboundedReceiver<crate::harness::HarnessEvent> {
        assert!(system_prompt.is_some_and(|prompt| prompt.contains("one file")));
        let (tx, rx) = futures::channel::mpsc::unbounded();
        for event in [
            crate::harness::HarnessEvent::ToolStarted {
                id: "t1".into(),
                name: "Glob".into(),
            },
            crate::harness::HarnessEvent::Finished {
                is_error: false,
                result: r#"{"files": ["notes.md"]}"#.into(),
            },
        ] {
            tx.unbounded_send(event).unwrap();
        }
        rx
    }

    /// A harness that finds two files.
    fn finds_both(
        _: String,
        _: Option<String>,
        _: std::path::PathBuf,
    ) -> futures::channel::mpsc::UnboundedReceiver<crate::harness::HarnessEvent> {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        tx.unbounded_send(crate::harness::HarnessEvent::Finished {
            is_error: false,
            result: r#"{"files": ["notes.md", "src/lib.rs"]}"#.into(),
        })
        .unwrap();
        rx
    }

    /// On the Files tab, Enter with nothing matched hands the search to the
    /// harness, and the one file it finds opens. Ctrl/Cmd+Enter hands it over
    /// even while files match; finding several leaves the palette showing the
    /// search under an alert.
    #[gpui_kit::test]
    async fn palette_hands_file_searches_to_the_harness(cx: &mut TestAppContext) {
        let dir =
            std::env::temp_dir().join(format!("suspense-palette-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("notes.md"), "# Notes\n").unwrap();
        std::fs::write(dir.join("src/lib.rs"), "").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;

        cx.update_window(handle, |_, window, cx| window.press(PALETTE, cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("palette").is_some()
        })
        .await;
        let palette = main.read_with(cx, |main, _| main.palette.clone().unwrap());
        palette.update(cx, |palette, _| palette.set_send_to_harness(finds_notes));
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            palette.read(cx).result_labels().len() == 2
        })
        .await;
        for key in "the meeting jottings".chars() {
            cx.update_window(handle, |_, window, cx| window.input(&key.to_string(), cx))
                .unwrap();
            cx.run_until_parked();
        }
        cx.update_window(handle, |_, window, cx| window.press("enter", cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, cx| {
            window.try_find("file-view").is_some() && !palette.read(cx).is_open(window, cx)
        })
        .await;

        cx.update_window(handle, |_, window, cx| window.press(PALETTE, cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            main.read(cx)
                .palette
                .as_ref()
                .is_some_and(|open| open != &palette)
        })
        .await;
        let palette = main.read_with(cx, |main, _| main.palette.clone().unwrap());
        palette.update(cx, |palette, _| palette.set_send_to_harness(finds_both));
        cx.update_window(handle, |_, window, cx| window.input("s", cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            !palette.read(cx).result_labels().is_empty()
        })
        .await;
        #[cfg(target_os = "macos")]
        let search = "cmd-enter";
        #[cfg(not(target_os = "macos"))]
        let search = "ctrl-enter";
        cx.update_window(handle, |_, window, cx| window.press(search, cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, cx| {
            let palette = palette.read(cx);
            palette.is_searching_files()
                && window.try_find("palette-search").is_some()
                && !palette.is_open(window, cx)
        })
        .await;

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Esc in the palette clears the search, then closes the palette and puts
    /// focus back in the chat input.
    #[gpui_kit::test]
    async fn escape_closes_the_palette(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;

        cx.update_window(handle, |_, window, cx| window.press(PALETTE, cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("palette").is_some()
        })
        .await;
        let palette = main.read_with(cx, |main, _| main.palette.clone().unwrap());
        cx.update_window(handle, |_, window, cx| window.input("x", cx))
            .unwrap();
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| window.press("escape", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert!(
                palette.read(cx).is_open(window, cx),
                "Esc closed the palette before clearing the search"
            );
        })
        .unwrap();

        cx.update_window(handle, |_, window, cx| window.press("escape", cx))
            .unwrap();
        let chat = main.read_with(cx, |main, cx| main.prompt_mode.read(cx).chat_input_view());
        cx.wait_for(handle, TIMEOUT, |window, cx| {
            window.try_find("palette").is_none() && chat.read(cx).is_focused(window, cx)
        })
        .await;
    }
}
