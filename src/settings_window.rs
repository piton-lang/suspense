//! The settings, shown in the main window's inset panel: the system prompt
//! each tab gives a prompt, for the open project. Every edit is saved straight
//! away to the project's `.suspense/system-prompts` (see
//! [`crate::system_prompts`]), and the prompts are read from there each time
//! the settings open, so an edit made by hand shows up.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::*;

use crate::chat_input::SendMode;
use crate::piton_syntax;
use crate::project_directory::ProjectDirectory;
use crate::system_prompts::{self, CODE_LOCATION, SPEC_LOCATION};

actions!(suspense, [OpenSettings]);

/// Emitted when the settings are closed.
pub struct CloseSettings;

/// How tall each prompt's editor is before it scrolls.
const EDITOR_HEIGHT: Pixels = px(120.);

/// Ctrl+, (Cmd+, on macOS) opens the settings from anywhere in the main
/// window, which handles the action.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-,", OpenSettings, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-,", OpenSettings, None),
    ]);
}

/// One mode's system prompt, as edited.
struct PromptEditor {
    mode: SendMode,
    editor: Entity<EditorState>,
    /// Why the prompt could not be read or saved, until it can be.
    error: Option<SharedString>,
}

pub struct SettingsWindow {
    prompts: Vec<PromptEditor>,
    /// Set while the editors are filled from disk, so doing so saves nothing.
    loading: bool,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CloseSettings> for SettingsWindow {}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SettingsWindow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = vec![
            cx.observe_global_in::<ProjectDirectory>(window, |this, window, cx| {
                this.load(window, cx)
            }),
        ];
        let prompts = SendMode::ALL
            .into_iter()
            .map(|mode| {
                let editor = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(piton_syntax::LANGUAGE_NAME)
                        .line_number(false)
                        .folding(false)
                        .soft_wrap(true)
                });
                subscriptions.push(cx.subscribe_in(
                    &editor,
                    window,
                    move |this, _, event: &InputEvent, _, cx| {
                        if matches!(event, InputEvent::Change) && !this.loading {
                            this.save(mode, cx);
                        }
                    },
                ));
                PromptEditor {
                    mode,
                    editor,
                    error: None,
                }
            })
            .collect();

        let mut this = Self {
            prompts,
            loading: false,
            scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.load(window, cx);
        this
    }

    /// Fills each editor with the open project's prompt, leaving alone any
    /// that already shows it so its cursor stays put.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project_dir = ProjectDirectory::get(cx);
        if let Some(project_dir) = &project_dir {
            // So each can be found and edited by hand too. The defaults still
            // show if they cannot be saved.
            system_prompts::save_missing(project_dir).ok();
        }
        self.loading = true;
        for prompt in &mut self.prompts {
            let (text, error) = match &project_dir {
                Some(project_dir) => match system_prompts::load(prompt.mode, project_dir) {
                    Ok(text) => (text, None),
                    Err(err) => (String::new(), Some(format!("{err:#}").into())),
                },
                None => (String::new(), None),
            };
            prompt.error = error;
            if prompt.editor.read(cx).value().as_ref() != text {
                prompt
                    .editor
                    .update(cx, |editor, cx| editor.set_value(text, window, cx));
            }
        }
        self.loading = false;
        cx.notify();
    }

    /// Saves `mode`'s prompt as edited.
    fn save(&mut self, mode: SendMode, cx: &mut Context<Self>) {
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        let Some(prompt) = self.prompts.iter_mut().find(|prompt| prompt.mode == mode) else {
            return;
        };
        let text = prompt.editor.read(cx).value();
        prompt.error = system_prompts::save(mode, &text, &project_dir)
            .err()
            .map(|err| format!("{err:#}").into());
        cx.notify();
    }

    /// Puts `mode`'s default prompt back, and saves it.
    fn reset(&mut self, mode: SendMode, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.prompts.iter().find(|prompt| prompt.mode == mode) else {
            return;
        };
        prompt.editor.update(cx, |editor, cx| {
            editor.set_value(system_prompts::default_template(mode), window, cx)
        });
        self.save(mode, cx);
    }

    fn render_prompt(&self, prompt: &PromptEditor, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mode = prompt.mode;
        let is_default =
            prompt.editor.read(cx).value().as_ref() == system_prompts::default_template(mode);
        let file = format!(".suspense/system-prompts/{}.md", mode.key());
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().font_semibold().child(mode.label()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(file),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "reset-{}-system-prompt",
                            mode.key()
                        )))
                        .ghost()
                        .xsmall()
                        .label("Reset to default")
                        .disabled(is_default)
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.reset(mode, window, cx)),
                        ),
                    ),
            )
            .child(Editor::new(&prompt.editor).h(EDITOR_HEIGHT))
            .children(
                prompt
                    .error
                    .clone()
                    .map(|error| div().text_sm().text_color(theme.danger).child(error)),
            )
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, foreground, muted, border) = (
            theme.background,
            theme.foreground,
            theme.muted_foreground,
            theme.border,
        );
        let body: AnyElement = if ProjectDirectory::get(cx).is_some() {
            let prompts: Vec<AnyElement> = self
                .prompts
                .iter()
                .map(|prompt| self.render_prompt(prompt, cx).into_any_element())
                .collect();
            v_flex().gap_5().children(prompts).into_any_element()
        } else {
            div()
                .text_color(muted)
                .child("Open a project to edit its system prompts.")
                .into_any_element()
        };

        let heading = h_flex()
            .flex_none()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(border)
            .child(div().flex_1().font_semibold().child("Settings"))
            .child(
                Button::new("settings-close")
                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Close the settings")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseSettings))),
            );
        let page = div()
            .id("settings")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .bg(background)
            .text_color(foreground)
            .child(
                v_flex()
                    .gap_4()
                    .p_6()
                    .child(div().text_lg().font_semibold().child("System prompts"))
                    .child(div().text_sm().text_color(muted).child(format!(
                        "Each tab gives a prompt sent from it this system prompt. \
                         They are saved with the project as they are edited. \
                         {CODE_LOCATION} and {SPEC_LOCATION} stand for codeRoot and \
                         root in piton.config.pi."
                    )))
                    .child(body),
            );
        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .bg(background)
            .child(heading)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(crate::scrollbar::with_scrollbar(
                        "settings",
                        &self.scroll,
                        page,
                        true,
                        None,
                        cx,
                    )),
            )
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: globbing `gpui_kit::*` would bring in GPUI's `test`
    // macro and shadow Rust's `#[test]`.
    use std::fs;

    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, Entity, Focusable as _, TestAppContext, VisualTestContext};

    use super::SettingsWindow;
    use crate::chat_input::SendMode;
    use crate::piton_syntax;
    use crate::project_directory::ProjectDirectory;
    use crate::system_prompts;

    fn text(
        settings: &Entity<SettingsWindow>,
        mode: SendMode,
        cx: &mut VisualTestContext,
    ) -> String {
        settings.read_with(cx, |this, cx| {
            let prompt = this
                .prompts
                .iter()
                .find(|prompt| prompt.mode == mode)
                .unwrap();
            prompt.editor.read(cx).value().to_string()
        })
    }

    /// The window shows each mode's prompt for the open project, saving the
    /// defaults so they can be edited by hand. An edit is saved as it is made,
    /// an edit made by hand shows once the prompts are read again, and a
    /// prompt can be put back to its default.
    #[gpui_kit::test]
    async fn edits_the_projects_system_prompts(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-settings-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            ProjectDirectory::set(dir.clone(), cx);
        });
        let mut settings = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| SettingsWindow::new(window, cx));
            settings = Some(view.clone());
            Root::new(view, window, cx)
        });
        let settings = settings.unwrap();
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();

        for mode in SendMode::ALL {
            assert_eq!(
                text(&settings, mode, cx),
                system_prompts::default_template(mode)
            );
            assert!(
                system_prompts::file(mode, &dir).exists(),
                "{mode:?} was not saved"
            );
        }

        settings.update_in(cx, |this, window, cx| {
            this.prompts[0]
                .editor
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
        });
        for key in "Hi.".chars() {
            cx.update(|window, cx| window.input(&key.to_string(), cx));
            cx.run_until_parked();
        }
        let typed = text(&settings, SendMode::Code, cx);
        assert!(typed.contains("Hi."), "{typed}");
        assert_eq!(system_prompts::load(SendMode::Code, &dir).unwrap(), typed);

        system_prompts::save(SendMode::Spec, "Edited by hand.", &dir).unwrap();
        settings.update_in(cx, |this, window, cx| this.load(window, cx));
        cx.run_until_parked();
        assert_eq!(text(&settings, SendMode::Spec, cx), "Edited by hand.");
        // Reading them again saves nothing over them.
        assert_eq!(system_prompts::load(SendMode::Code, &dir).unwrap(), typed);

        settings.update_in(cx, |this, window, cx| {
            this.reset(SendMode::Spec, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            system_prompts::load(SendMode::Spec, &dir).unwrap(),
            system_prompts::default_template(SendMode::Spec)
        );

        fs::remove_dir_all(&dir).ok();
    }
}
