//! A text input that grows to fit its text: a row for each visual line, lines
//! broken with Enter and long lines that soft wrap alike, from a single row up
//! to a most, after which it scrolls. It shrinks as text goes, and fits again
//! when its width changes. Whatever shows the input keeps a [`GrowToFit`],
//! sizes the editor with [`GrowToFit::heights`], and puts
//! [`GrowToFit::tracker`] over it, so the fit follows how the editor is laid
//! out.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::EditorState;
use gpui_kit::*;

/// Room the editor keeps free at the right of each line when soft wrapping.
const WRAP_RIGHT_MARGIN: Pixels = px(10.);

/// How an input is fitted to its text, from how its editor was last laid out.
pub struct GrowToFit {
    max_rows: usize,
    /// Width the editor's text was last laid out in, which decides where long
    /// lines soft wrap.
    text_width: Option<Pixels>,
    /// What the editor adds around its rows (padding and border) as last
    /// laid out, rounded to device pixels.
    chrome: Option<Pixels>,
    /// The font the editor draws its text in, as last laid out: its family
    /// and size are the theme's monospace ones, but its weight, features, and
    /// fallbacks come from wherever the editor sits.
    font: Option<Font>,
    /// How many rows the editor laid a text out in, at a width, when that
    /// differed from the count made ahead of drawing it.
    laid_out: Option<LaidOut>,
    /// Rows the count made ahead of drawing is off by, to stand in for a font
    /// that wraps differently from the editor's.
    #[cfg(test)]
    miscount: isize,
}

/// Rows the editor laid a text out in, at the width it wrapped in.
struct LaidOut {
    text: SharedString,
    width: Pixels,
    rows: usize,
}

impl GrowToFit {
    /// Grows up to `max_rows` rows.
    pub fn new(max_rows: usize) -> Self {
        Self {
            max_rows,
            text_width: None,
            chrome: None,
            font: None,
            laid_out: None,
            #[cfg(test)]
            miscount: 0,
        }
    }

    /// How tall the editor is for its text, and how tall a single row of it
    /// is, for lining things up with it.
    pub fn heights(
        &self,
        editor: &Entity<EditorState>,
        window: &Window,
        cx: &App,
    ) -> (Pixels, Pixels) {
        let text = editor.read(cx).value();
        let rows = self.rows(&text, window, cx).clamp(1, self.max_rows);
        // Fit the text with the editor's own metrics: its laid-out line height
        // plus the padding and border around its rows. Until the editor has
        // painted, the border is estimated at a device pixel or more per side.
        let line_height = editor
            .read(cx)
            .line_height()
            .unwrap_or_else(|| window.line_height());
        let chrome = self.chrome.unwrap_or_else(|| {
            gpui_kit::component::Size::Medium.input_py() * 2.
                + ceil_to_device_pixel(px(1.), window) * 2.
        });
        (
            ceil_to_device_pixel(line_height * rows as f32 + chrome, window),
            ceil_to_device_pixel(line_height + chrome, window),
        )
    }

    /// The rows `text` takes in the editor: as the editor laid it out, once
    /// it has, or else as counted ahead of drawing it.
    fn rows(&self, text: &SharedString, window: &Window, cx: &App) -> usize {
        match &self.laid_out {
            Some(laid_out) if &laid_out.text == text && Some(laid_out.width) == self.text_width => {
                laid_out.rows
            }
            _ => {
                let rows = self.visual_rows(text, window, cx);
                #[cfg(test)]
                let rows = rows.saturating_add_signed(self.miscount).max(1);
                rows
            }
        }
    }

    /// The rows `text` takes in the editor: one per line, plus one for every
    /// soft wrap, wrapped the way the editor wraps it, in its font.
    fn visual_rows(&self, text: &str, window: &Window, cx: &App) -> usize {
        let Some(wrap_width) = self.text_width.map(|width| width - WRAP_RIGHT_MARGIN) else {
            return text.split('\n').count();
        };
        let theme = cx.theme();
        let font = self
            .font
            .clone()
            .unwrap_or_else(|| font(theme.mono_font_family.clone()));
        let mut wrapper = window
            .text_system()
            .line_wrapper(font, theme.mono_font_size);
        text.split('\n')
            .map(|line| {
                1 + wrapper
                    .wrap_line(&[LineFragment::text(line)], wrap_width)
                    .count()
            })
            .sum()
    }

    /// Laid over the editor: once the editor has painted, checks how it was
    /// laid out, the width its text wrapped in, the padding and border around
    /// its rows, the font it drew in, and how many rows its text took, and
    /// when any of them differ from what the fit was counted with, has
    /// `owner`, which keeps the fit where `fit` finds it, draw again to fit
    /// anew.
    pub fn tracker<T: 'static>(
        editor: &Entity<EditorState>,
        owner: WeakEntity<T>,
        fit: impl Fn(&mut T) -> &mut GrowToFit + 'static,
    ) -> impl IntoElement {
        let editor = editor.clone();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                let Some(text_bounds) = editor.read(cx).text_bounds() else {
                    return;
                };
                let width = text_bounds.size.width;
                let chrome = bounds.size.height - text_bounds.size.height;
                // The editor sets only the family and size of the text style
                // it sits in, so this is the font it drew in.
                let theme = cx.theme();
                let mut drawn_font = window.text_style().font();
                drawn_font.family = theme.mono_font_family.clone();
                let laid_out = laid_out_rows(&editor, cx);
                let text = editor.read(cx).value();
                owner
                    .update(cx, |owner, cx| {
                        let fit = fit(owner);
                        let chrome_changed =
                            fit.chrome.is_none_or(|old| (old - chrome).abs() > px(0.01));
                        let mut changed = false;
                        if fit.text_width != Some(width) || chrome_changed {
                            fit.text_width = Some(width);
                            if chrome_changed {
                                fit.chrome = Some(chrome);
                            }
                            changed = true;
                        }
                        if fit.font.as_ref() != Some(&drawn_font) {
                            fit.font = Some(drawn_font);
                            changed = true;
                        }
                        // Where the rows counted ahead differ from those laid
                        // out, the laid out ones are taken for this text.
                        if let Some(rows) = laid_out {
                            let shown = fit.rows(&text, window, cx).clamp(1, fit.max_rows);
                            let within = rows.min(fit.max_rows);
                            if within != shown {
                                fit.laid_out = Some(LaidOut { text, width, rows });
                                changed = true;
                            }
                        }
                        if changed {
                            cx.notify();
                        }
                    })
                    .ok();
            },
        )
        .absolute()
        .size_full()
    }
}

/// How many rows the editor laid its text out in, from where it placed the
/// start of the text and the end: `None` before it is laid out. An end the
/// editor didn't lay out, being out of its view, counts as a row more than it
/// showed, so the input grows until the end is in view.
fn laid_out_rows(editor: &Entity<EditorState>, cx: &App) -> Option<usize> {
    let editor = editor.read(cx);
    let line_height = editor.line_height()?;
    let first = editor.range_to_bounds(&(0..0))?;
    let end = editor.value().len();
    match editor.range_to_bounds(&(end..end)) {
        Some(last) => Some(
            ((last.bottom() - first.top()) / line_height)
                .round()
                .max(1.) as usize,
        ),
        None => {
            let shown = editor.visible_row_range()?;
            Some(shown.end + 1)
        }
    }
}

/// `length` rounded up to a whole device pixel. The editor lays its rows out
/// on device pixels, and rounds a height that falls between device pixels
/// half toward zero, so at fractional scales an input sized in logical pixels
/// can come out a fraction of a pixel short of its rows; the editor then takes
/// its last row for out of view and scrolls it in for a frame, which flickers
/// the text a line up and back.
pub fn ceil_to_device_pixel(length: Pixels, window: &Window) -> Pixels {
    let scale_factor = window.scale_factor();
    // Float noise just past a whole device pixel must not round up to the next.
    px((f32::from(length) * scale_factor - 1e-3).ceil() / scale_factor)
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::Root;
    use gpui_kit::component::input::{Editor, EditorState};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
        TestAppContext, Window, div,
    };

    use super::GrowToFit;

    struct Fitted {
        editor: Entity<EditorState>,
        fit: GrowToFit,
    }

    impl Render for Fitted {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let (height, _) = self.fit.heights(&self.editor, window, cx);
            div()
                .relative()
                .w_full()
                .child(Editor::new(&self.editor).h(height))
                .child(GrowToFit::tracker(
                    &self.editor,
                    cx.entity().downgrade(),
                    |this: &mut Self| &mut this.fit,
                ))
        }
    }

    /// Whatever the count made ahead of drawing says, over or under, the
    /// input settles on the rows the editor laid the text out in: every row
    /// in view, and none empty.
    #[gpui_kit::test]
    async fn settles_on_the_rows_laid_out(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        for miscount in [-2isize, 2] {
            let mut view = None;
            let window = cx.add_window(|window, cx| {
                let editor = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .soft_wrap(true)
                        .line_number(false)
                        .scroll_beyond_last_line(Some(0))
                });
                let fitted = cx.new(|_| {
                    let mut fit = GrowToFit::new(20);
                    fit.miscount = miscount;
                    Fitted { editor, fit }
                });
                view = Some(fitted.clone());
                Root::new(fitted, window, cx)
            });
            let view = view.unwrap();
            let handle = window.into();
            let editor = view.read_with(cx, |view, _| view.editor.clone());
            cx.update_window(handle, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    editor.set_value("one\ntwo\nthree\nfour\nfive", window, cx)
                });
                for _ in 0..6 {
                    window.render_frame(cx);
                }
                let editor = editor.read(cx);
                let line_height = editor.line_height().unwrap();
                let text = editor.text_bounds().unwrap();
                let end = editor.value().len();
                let last = editor.range_to_bounds(&(end..end)).unwrap();
                assert_eq!(
                    editor.scroll_offset().y,
                    gpui_kit::px(0.),
                    "miscount {miscount}: scrolled"
                );
                assert!(
                    (text.bottom() - last.bottom()).abs() < line_height / 2.,
                    "miscount {miscount}: the text ends at {:?}, the input at {:?}",
                    last.bottom(),
                    text.bottom()
                );
            })
            .unwrap();
        }
    }
}
