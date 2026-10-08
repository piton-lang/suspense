//! About Suspense: which version is running and what it was built from, as
//! the AboutScope says. Everything shown is baked into the binary when it is
//! built, but for the version of piton on the machine, which `piton
//! --version` says once the panel opens.

use crate::process::Logged as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::self_update::{self, REPOSITORY, Updates};
use crate::version::VERSION;

actions!(suspense, [OpenAbout]);

/// Emitted to close the panel.
pub struct CloseAbout;

/// The commit the build was made from, its short hash, where the build
/// recorded one (see `build.rs`).
pub const COMMIT: Option<&str> = option_env!("SUSPENSE_COMMIT");

/// The date the build was made, where it recorded one (see `build.rs`).
pub const BUILT: Option<&str> = option_env!("SUSPENSE_BUILT");

/// How long Copy details reads "Copied" after it is pressed.
const COPIED_FOR: Duration = Duration::from_secs(2);

/// The application's icon, as its packaging ships it.
const ICON: &[u8] = include_bytes!("../packaging/icons/suspense-128.png");

/// What it says it is, beneath its name.
const DESCRIPTION: &str =
    "A desktop application for writing a Piton spec and the code it describes with a coding agent";

/// Whether this is an edge build, published from a repository with an edge
/// release's version, or one made elsewhere, as on a developer's machine.
pub fn is_edge() -> bool {
    REPOSITORY.is_some() && self_update::sequence(VERSION).is_some()
}

/// The operating system and architecture it was built for, as "Windows
/// x86-64" or "macOS arm64".
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86-64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{os} {arch}")
}

/// What `piton --version` prints, trimmed; none where piton can't be run.
fn piton_version() -> Option<String> {
    let output = crate::process::command("piton")
        .arg("--version")
        .output_logged()
        .ok()
        .filter(|output| output.status.success())?;
    let printed = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!printed.is_empty()).then_some(printed)
}

/// A figure's row: its label, its value, and, muted after it, what
/// qualifies it.
#[derive(Clone, Debug, PartialEq)]
pub struct Figure {
    pub label: &'static str,
    pub value: String,
    pub note: Option<&'static str>,
}

/// The figures, in order, those the build doesn't record left out; piton's
/// as `piton`, `None` while it is still being checked.
pub fn figures(piton: Option<&Option<String>>) -> Vec<Figure> {
    let mut figures = vec![Figure {
        label: "Version",
        value: VERSION.to_string(),
        note: Some(if is_edge() {
            "edge"
        } else {
            "development build"
        }),
    }];
    if let Some(commit) = COMMIT {
        figures.push(Figure {
            label: "Commit",
            value: commit.to_string(),
            note: None,
        });
    }
    if let Some(built) = BUILT {
        figures.push(Figure {
            label: "Built",
            value: built.to_string(),
            note: None,
        });
    }
    figures.push(Figure {
        label: "Platform",
        value: platform(),
        note: None,
    });
    figures.push(Figure {
        label: "Piton",
        value: match piton {
            None => "Checking…".to_string(),
            Some(Some(version)) => version.clone(),
            Some(None) => "Not installed".to_string(),
        },
        note: None,
    });
    figures
}

/// The figures as plain text for a bug report: "Suspense", then one
/// "Label: value" per line.
pub fn details_text(figures: &[Figure]) -> String {
    let mut text = String::from("Suspense\n");
    for figure in figures {
        text.push_str(figure.label);
        text.push_str(": ");
        text.push_str(&figure.value);
        if let Some(note) = figure.note {
            text.push_str(&format!(" ({note})"));
        }
        text.push('\n');
    }
    text
}

pub struct AboutView {
    /// What `piton --version` said, once it has: none where it couldn't.
    piton: Option<Option<String>>,
    copied_at: Option<Instant>,
    icon: Arc<Image>,
    focus_handle: FocusHandle,
    _check: Task<()>,
}

impl EventEmitter<CloseAbout> for AboutView {}

impl Focusable for AboutView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl AboutView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Asked once the panel opens, never holding it back.
        let check = cx.background_spawn(async { piton_version() });
        let check = cx.spawn(async move |this, cx| {
            let version = check.await;
            this.update(cx, |this, cx| {
                this.piton = Some(version);
                cx.notify();
            })
            .ok();
        });
        Self {
            piton: None,
            copied_at: None,
            icon: Arc::new(Image::from_bytes(ImageFormat::Png, ICON.to_vec())),
            focus_handle: cx.focus_handle(),
            _check: check,
        }
    }

    fn copy_details(&mut self, cx: &mut Context<Self>) {
        let text = details_text(&figures(self.piton.as_ref()));
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied_at = Some(Instant::now());
        // It reads "Copied" for a while, then as before.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
        cx.notify();
    }
}

impl Render for AboutView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let copied = self
            .copied_at
            .is_some_and(|at| at.elapsed() < COPIED_FOR);

        let heading = h_flex()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(div().flex_1().font_semibold().child("About Suspense"))
            .child(
                Button::new("about-close-x")
                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseAbout))),
            );

        let rows = figures(self.piton.as_ref()).into_iter().map(|figure| {
            let id = SharedString::from(format!("about-{}", figure.label.to_lowercase()));
            gpui_kit::TestSupportExt::test_support(
                h_flex()
                    .id(id)
                    .justify_between()
                    .gap_4()
                    .child(div().text_color(muted).child(figure.label))
                    .child(
                        h_flex()
                            .gap_1()
                            .font_features(crate::subagents::tabular_figures())
                            .child(figure.value)
                            .when_some(figure.note, |row, note| {
                                row.child(div().text_color(muted).child(note))
                            }),
                    ),
            )
        });

        let links = REPOSITORY.filter(|_| is_edge()).map(|repository| {
            let repo_url = format!("https://github.com/{repository}");
            let notes_url = format!("{repo_url}/releases/tag/v{VERSION}");
            h_flex()
                .gap_4()
                .justify_center()
                .child(
                    Button::new("about-repository")
                        .link()
                        .small()
                        .label(repository)
                        .on_click(move |_, _, cx| cx.open_url(&repo_url)),
                )
                .child(
                    Button::new("about-release-notes")
                        .link()
                        .small()
                        .label("Release notes")
                        .on_click(move |_, _, cx| cx.open_url(&notes_url)),
                )
        });

        let (update_enabled, update_tooltip) = Updates::ribbon_state(cx);
        let body = v_flex()
            .id("about-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .items_center()
            .p_6()
            .child(
                v_flex()
                    .w(px(380.))
                    .max_w_full()
                    .gap_4()
                    .items_center()
                    .child(img(ImageSource::Image(self.icon.clone())).size(px(64.)))
                    .child(div().text_2xl().font_semibold().child("Suspense"))
                    .child(
                        div()
                            .text_sm()
                            .text_center()
                            .text_color(muted)
                            .whitespace_normal()
                            .child(DESCRIPTION),
                    )
                    .child(
                        v_flex()
                            .w_full()
                            .gap_1p5()
                            .pt_2()
                            .text_sm()
                            .children(rows),
                    )
                    .children(links),
            );

        let footer = h_flex()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(theme.border)
            .child(
                Button::new("about-copy")
                    .icon(if copied {
                        IconName::Check
                    } else {
                        IconName::Copy
                    })
                    .label(if copied { "Copied" } else { "Copy details" })
                    .on_click(cx.listener(|this, _, _, cx| this.copy_details(cx))),
            )
            .when(self_update::cant_update().is_none(), |footer| {
                footer.child(
                    Button::new("about-check-for-updates")
                        .icon(IconName::RefreshCw)
                        .label("Check for Updates")
                        .tooltip(update_tooltip)
                        .loading(Updates::busy(cx))
                        .disabled(!update_enabled)
                        .on_click(|_, _, cx| Updates::check_from_ribbon(cx)),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("about-close")
                    .primary()
                    .label("Close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseAbout))),
            );

        let view = v_flex()
            .id("about")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .child(heading)
            .child(body)
            .child(footer);
        // Lets UI tests find the panel; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(view)
    }
}

#[cfg(test)]
mod tests {
    use super::{Figure, details_text, figures, platform};

    /// The figures come in order, the version first and piton last, its row
    /// "Checking…" until piton has answered.
    #[test]
    fn the_figures_are_in_order() {
        let checking = figures(None);
        let labels: Vec<_> = checking.iter().map(|figure| figure.label).collect();
        assert_eq!(labels.first(), Some(&"Version"));
        assert_eq!(&labels[labels.len() - 2..], ["Platform", "Piton"]);
        assert_eq!(checking.last().unwrap().value, "Checking…");
        assert_eq!(
            figures(Some(&None)).last().unwrap().value,
            "Not installed"
        );
        assert_eq!(
            figures(Some(&Some("piton 1.2.3".into()))).last().unwrap().value,
            "piton 1.2.3"
        );
        assert!(platform().contains(' '));
    }

    /// Copied for a bug report, the details read "Suspense" first, then one
    /// "Label: value" per line.
    #[test]
    fn details_copy_as_plain_text() {
        let text = details_text(&[
            Figure {
                label: "Version",
                value: "0.1.42".into(),
                note: Some("edge"),
            },
            Figure {
                label: "Piton",
                value: "Not installed".into(),
                note: None,
            },
        ]);
        assert_eq!(text, "Suspense\nVersion: 0.1.42 (edge)\nPiton: Not installed\n");
    }
}
