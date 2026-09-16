//! The project indicator: which project is loaded, shown as its folder's name
//! after a folder icon, or "No project" while none is, with the project's path
//! as its tooltip. Clicking it opens a list of the projects opened most
//! recently, any of which opens with a click, ending in "Open Project…", which
//! opens a project browser.

use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::main_window::FocusChat;
use crate::project::open_project::OpenProject;
use crate::project_directory::ProjectDirectory;
use crate::recent_projects;

/// The widest the project's name gets before it is cut off.
const MAX_NAME_WIDTH: Pixels = px(320.);

/// A folder's name, or its whole path when it has none.
fn folder_name(dir: &std::path::Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// Emitted, as [`OpenProject`], when "Open Project…" is picked from the list.
impl EventEmitter<OpenProject> for ProjectIndicator {}

pub struct ProjectIndicator {
    /// Whether the list of recent projects is open.
    open: bool,
    /// The list's, which takes focus while it is open, for <Escape>.
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl ProjectIndicator {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            open: false,
            focus_handle: cx.focus_handle(),
            // Always the project open now.
            _subscription: cx.observe_global::<ProjectDirectory>(|_, cx| cx.notify()),
        }
    }

    /// Whether the list of recent projects is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Closes the list, if it is open.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            cx.notify();
        }
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let current = ProjectDirectory::get(cx);
        let recent = recent_projects::get(cx).openable();
        let row = |id: ElementId| {
            h_flex()
                .id(id)
                .gap_2()
                .px_3()
                .py_1p5()
                .rounded(theme.radius)
                .cursor_pointer()
                .hover(|row| row.bg(theme.list_hover))
        };
        let rows = recent.into_iter().enumerate().map(|(ix, dir)| {
            let is_current = current.as_deref() == Some(dir.as_path());
            let path = dir.display().to_string();
            let name = folder_name(&dir);
            let item = row(("recent-project", ix).into())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.close(cx);
                    if !is_current {
                        ProjectDirectory::set(dir.clone(), cx);
                    }
                }))
                .child(
                    Icon::new(if is_current {
                        IconName::Check
                    } else {
                        IconName::FolderClosed
                    })
                    .small()
                    .text_color(theme.muted_foreground),
                )
                .child(
                    v_flex()
                        .min_w_0()
                        .child(div().truncate().font_medium().child(name))
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(path),
                        ),
                );
            // Lets UI tests find the row; inert in normal builds.
            gpui_kit::TestSupportExt::test_support(item)
        });
        let open_project = row("open-project-from-recent".into())
            .border_t_1()
            .border_color(theme.border)
            .on_click(cx.listener(|this, _, _, cx| {
                this.close(cx);
                cx.emit(OpenProject);
            }))
            .child(
                Icon::new(IconName::FolderOpen)
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .child("Open Project…");
        let list = v_flex()
            .id("recent-projects")
            .w(px(360.))
            .mt(px(28.))
            .p_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_md()
            .occlude()
            .track_focus(&self.focus_handle)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close(cx)))
            .on_action(cx.listener(|this, _: &FocusChat, _, cx| this.close(cx)))
            .children(rows)
            .child(gpui_kit::TestSupportExt::test_support(open_project));
        // Lets UI tests find the list; inert in normal builds.
        deferred(
            anchored()
                .snap_to_window()
                .child(gpui_kit::TestSupportExt::test_support(list)),
        )
        .with_priority(2)
    }
}

impl Render for ProjectIndicator {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let project = ProjectDirectory::get(cx);
        let name = project.as_deref().map(folder_name);
        let label = h_flex()
            .id("ribbon-project-name")
            .min_w_0()
            .max_w(MAX_NAME_WIDTH)
            .gap_1p5()
            .px_3()
            .py_0p5()
            .rounded(theme.radius)
            .cursor_pointer()
            .hover(|label| label.bg(theme.list_hover))
            .on_click(cx.listener(|this, _, window, cx| {
                this.open = !this.open;
                if this.open {
                    this.focus_handle.focus(window, cx);
                }
                cx.notify();
            }))
            .child(
                Icon::new(IconName::FolderClosed)
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .map(|label| match name {
                Some(name) => label.child(div().min_w_0().truncate().font_medium().child(name)),
                None => label.child(div().text_color(theme.muted_foreground).child("No project")),
            })
            .when_some(project, |label, dir| {
                let path = SharedString::from(dir.display().to_string());
                label.tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
            });
        let list = self.open.then(|| self.render_list(cx));
        // Lets UI tests find the name; inert in normal builds.
        div()
            .flex_none()
            .child(gpui_kit::TestSupportExt::test_support(label))
            .children(list)
    }
}
