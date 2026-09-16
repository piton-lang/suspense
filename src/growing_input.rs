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
}

impl GrowToFit {
    /// Grows up to `max_rows` rows.
    pub fn new(max_rows: usize) -> Self {
        Self {
            max_rows,
            text_width: None,
            chrome: None,
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
        let rows = self.visual_rows(&text, window, cx).clamp(1, self.max_rows);
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

    /// The rows `text` takes in the editor: one per line, plus one for every
    /// soft wrap, wrapped the way the editor wraps it.
    fn visual_rows(&self, text: &str, window: &Window, cx: &App) -> usize {
        let Some(wrap_width) = self.text_width.map(|width| width - WRAP_RIGHT_MARGIN) else {
            return text.split('\n').count();
        };
        let theme = cx.theme();
        let mut wrapper = window
            .text_system()
            .line_wrapper(font(theme.mono_font_family.clone()), theme.mono_font_size);
        text.split('\n')
            .map(|line| {
                1 + wrapper
                    .wrap_line(&[LineFragment::text(line)], wrap_width)
                    .count()
            })
            .sum()
    }

    /// Laid over the editor: once the editor has painted, checks how it was
    /// laid out, the width its text wrapped in and the padding and border
    /// around its rows, and when either changed, has `owner`, which keeps the
    /// fit where `fit` finds it, draw again to fit anew.
    pub fn tracker<T: 'static>(
        editor: &Entity<EditorState>,
        owner: WeakEntity<T>,
        fit: fn(&mut T) -> &mut GrowToFit,
    ) -> impl IntoElement {
        let editor = editor.clone();
        canvas(
            |_, _, _| {},
            move |bounds, _, _, cx| {
                let Some(text_bounds) = editor.read(cx).text_bounds() else {
                    return;
                };
                let width = text_bounds.size.width;
                let chrome = bounds.size.height - text_bounds.size.height;
                owner
                    .update(cx, |owner, cx| {
                        let fit = fit(owner);
                        let chrome_changed =
                            fit.chrome.is_none_or(|old| (old - chrome).abs() > px(0.01));
                        if fit.text_width != Some(width) || chrome_changed {
                            fit.text_width = Some(width);
                            if chrome_changed {
                                fit.chrome = Some(chrome);
                            }
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
