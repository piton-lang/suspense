//! A line a command printed, read as a terminal would show it: its escape
//! sequences and control characters taken out, a line rewritten with a
//! carriage return shown as last written, and the colours and emphasis it
//! wrote kept as runs, drawn in the theme's colours rather than the
//! terminal's.

use std::ops::Range;

use gpui_kit::{App, FontStyle, FontWeight, HighlightStyle, SharedString, UnderlineStyle, px};

use crate::theme::{Palette, color, palette};

/// Columns between tab stops.
const TAB_WIDTH: usize = 4;

/// A colour a command wrote, by the hue the theme draws it in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hue {
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    /// Black and dark greys.
    Dark,
    /// White and light greys.
    Light,
}

/// How a run of a line is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub hue: Option<Hue>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
}

impl Style {
    fn is_plain(&self) -> bool {
        *self == Style::default()
    }

    fn highlight(&self, palette: &Palette) -> HighlightStyle {
        let hue = self.hue.map(|hue| match hue {
            Hue::Red => palette.error,
            Hue::Green => palette.success,
            Hue::Yellow => palette.warning,
            Hue::Blue => palette.accent,
            Hue::Magenta => palette.purple,
            Hue::Cyan => palette.cyan,
            Hue::Dark => palette.text_tertiary,
            Hue::Light => palette.text,
        });
        let hue = hue.or(self.dim.then_some(palette.text_tertiary));
        HighlightStyle {
            color: hue.map(color),
            font_weight: self.bold.then_some(FontWeight::BOLD),
            font_style: self.italic.then_some(FontStyle::Italic),
            underline: self.underline.then_some(UnderlineStyle {
                thickness: px(1.),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

/// A line as it shows: its text, and the styled runs of it, by byte range.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConsoleLine {
    pub text: SharedString,
    pub runs: Vec<(Range<usize>, Style)>,
}

impl ConsoleLine {
    /// Reads a line as printed, without its line break.
    pub fn parse(raw: &str) -> Self {
        let mut cells: Vec<(char, Style)> = Vec::new();
        let mut column = 0;
        let mut style = Style::default();
        let mut chars = raw.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\u{1b}' => match chars.next() {
                    // CSI: parameters, then a final byte from @ to ~.
                    Some('[') => {
                        let mut params = String::new();
                        let mut last = None;
                        for c in chars.by_ref() {
                            if ('\u{40}'..='\u{7e}').contains(&c) {
                                last = Some(c);
                                break;
                            }
                            params.push(c);
                        }
                        match last {
                            Some('m') => apply_sgr(&mut style, &params),
                            // Erasing the whole line starts it afresh.
                            Some('K') if params == "2" => {
                                cells.clear();
                                column = 0;
                            }
                            Some('G') => {
                                column = params.parse::<usize>().unwrap_or(1).saturating_sub(1)
                            }
                            _ => {}
                        }
                    }
                    // OSC: up to BEL or ESC \.
                    Some(']') => {
                        while let Some(c) = chars.next() {
                            if c == '\u{7}' {
                                break;
                            }
                            if c == '\u{1b}' {
                                if chars.peek() == Some(&'\\') {
                                    chars.next();
                                }
                                break;
                            }
                        }
                    }
                    // A character set or other two-byte sequence.
                    Some('(' | ')' | '*' | '+') => {
                        chars.next();
                    }
                    _ => {}
                },
                // Written over from the start of the line, as a progress
                // bar is.
                '\r' => column = 0,
                '\u{8}' => column = column.saturating_sub(1),
                '\t' => {
                    let to = (column / TAB_WIDTH + 1) * TAB_WIDTH;
                    while column < to {
                        put(&mut cells, &mut column, ' ', style);
                    }
                }
                // Not text: the replacement for bytes that weren't, and other
                // control characters.
                '\u{fffd}' => {}
                c if c.is_control() => {}
                c => put(&mut cells, &mut column, c, style),
            }
        }
        // Trailing blanks written by a carriage return's overwrite are kept
        // only as far as text reaches.
        while cells.last().is_some_and(|(c, s)| *c == ' ' && s.is_plain()) {
            cells.pop();
        }
        let mut line = Self::from_cells(&cells);
        if line.runs.is_empty() {
            line.runs = compiler_label(&line.text);
        }
        line
    }

    fn from_cells(cells: &[(char, Style)]) -> Self {
        let mut text = String::new();
        let mut runs: Vec<(Range<usize>, Style)> = Vec::new();
        for (c, style) in cells {
            let start = text.len();
            text.push(*c);
            if style.is_plain() {
                continue;
            }
            match runs.last_mut() {
                Some((range, last)) if last == style && range.end == start => {
                    range.end = text.len()
                }
                _ => runs.push((start..text.len(), *style)),
            }
        }
        Self {
            text: text.into(),
            runs,
        }
    }

    /// Its runs as highlights, in the theme's colours for the mode showing.
    pub fn highlights(&self, cx: &App) -> Vec<(Range<usize>, HighlightStyle)> {
        let palette = palette(cx);
        self.runs
            .iter()
            .map(|(range, style)| (range.clone(), style.highlight(palette)))
            .collect()
    }
}

/// Writes `c` at `column`, over whatever was there.
fn put(cells: &mut Vec<(char, Style)>, column: &mut usize, c: char, style: Style) {
    while cells.len() < *column {
        cells.push((' ', Style::default()));
    }
    if *column < cells.len() {
        cells[*column] = (c, style);
    } else {
        cells.push((c, style));
    }
    *column += 1;
}

/// Applies the SGR parameters `params`, such as `1;31`, to `style`.
fn apply_sgr(style: &mut Style, params: &str) {
    let codes: Vec<u16> = if params.is_empty() {
        vec![0]
    } else {
        params
            .split([';', ':'])
            .map(|code| code.parse().unwrap_or(0))
            .collect()
    };
    let mut codes = codes.into_iter();
    while let Some(code) = codes.next() {
        match code {
            0 => *style = Style::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 => style.underline = true,
            22 => (style.bold, style.dim) = (false, false),
            23 => style.italic = false,
            24 => style.underline = false,
            30..=37 => style.hue = Some(basic_hue(code - 30)),
            90..=97 => style.hue = Some(basic_hue(code - 90)),
            39 => style.hue = None,
            38 => style.hue = extended_hue(&mut codes),
            // Backgrounds are left out, the extended ones' values skipped.
            48 | 58 => {
                extended_hue(&mut codes);
            }
            _ => {}
        }
    }
}

fn basic_hue(ix: u16) -> Hue {
    match ix {
        0 => Hue::Dark,
        1 => Hue::Red,
        2 => Hue::Green,
        3 => Hue::Yellow,
        4 => Hue::Blue,
        5 => Hue::Magenta,
        6 => Hue::Cyan,
        _ => Hue::Light,
    }
}

/// The hue of a 256-colour (`5;n`) or true-colour (`2;r;g;b`) value.
fn extended_hue(codes: &mut impl Iterator<Item = u16>) -> Option<Hue> {
    match codes.next()? {
        5 => {
            let n = codes.next()?;
            match n {
                0..=7 => Some(basic_hue(n)),
                8..=15 => Some(basic_hue(n - 8)),
                16..=231 => {
                    let n = n - 16;
                    let level = |v: u16| if v == 0 { 0 } else { 55 + v * 40 };
                    Some(rgb_hue(level(n / 36), level(n / 6 % 6), level(n % 6)))
                }
                _ => Some(if n < 244 { Hue::Dark } else { Hue::Light }),
            }
        }
        2 => {
            let (r, g, b) = (codes.next()?, codes.next()?, codes.next()?);
            Some(rgb_hue(r, g, b))
        }
        _ => None,
    }
}

/// The nearest of the hues to a colour.
fn rgb_hue(r: u16, g: u16, b: u16) -> Hue {
    let (r, g, b) = (r as f32 / 255., g as f32 / 255., b as f32 / 255.);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max - min < 0.15 {
        return if max < 0.5 { Hue::Dark } else { Hue::Light };
    }
    let delta = max - min;
    let hue = if max == r {
        60. * ((g - b) / delta).rem_euclid(6.)
    } else if max == g {
        60. * ((b - r) / delta + 2.)
    } else {
        60. * ((r - g) / delta + 4.)
    };
    match hue {
        h if !(20. ..345.).contains(&h) => Hue::Red,
        h if h < 70. => Hue::Yellow,
        h if h < 160. => Hue::Green,
        h if h < 200. => Hue::Cyan,
        h if h < 260. => Hue::Blue,
        _ => Hue::Magenta,
    }
}

/// On a line written without colour, the label a compiler would colour:
/// `error`, `warning`, `note`, or `help`, then a colon or a bracket.
fn compiler_label(text: &str) -> Vec<(Range<usize>, Style)> {
    let start = text.len() - text.trim_start().len();
    let rest = &text[start..];
    [
        ("error", Hue::Red),
        ("warning", Hue::Yellow),
        ("note", Hue::Blue),
        ("help", Hue::Blue),
    ]
    .into_iter()
    .find(|(word, _)| {
        rest.strip_prefix(word)
            .is_some_and(|after| after.starts_with([':', '[']))
    })
    .map(|(word, hue)| {
        let style = Style {
            hue: Some(hue),
            bold: true,
            ..Style::default()
        };
        vec![(start..start + word.len(), style)]
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{ConsoleLine, Hue, Style};

    fn styled(line: &ConsoleLine) -> Vec<(&str, Style)> {
        line.runs
            .iter()
            .map(|(range, style)| (&line.text[range.clone()], *style))
            .collect()
    }

    #[test]
    fn escape_sequences_never_show() {
        let line = ConsoleLine::parse(
            "\u{1b}[0m\u{1b}[1m\u{1b}[32m   Compiling\u{1b}[0m demo v0.1.0\u{1b}[K",
        );
        assert_eq!(line.text.as_ref(), "   Compiling demo v0.1.0");
        assert_eq!(
            styled(&line),
            [(
                "   Compiling",
                Style {
                    hue: Some(Hue::Green),
                    bold: true,
                    ..Style::default()
                }
            )]
        );
        // A title, a link, and stray control characters and bytes.
        let line = ConsoleLine::parse(
            "\u{1b}]0;title\u{7}see \u{1b}]8;;http://x\u{1b}\\here\u{1b}]8;;\u{1b}\\\u{0}\u{fffd}!",
        );
        assert_eq!(line.text.as_ref(), "see here!");
        assert!(line.runs.is_empty());
    }

    #[test]
    fn a_rewritten_line_shows_as_last_written() {
        let line = ConsoleLine::parse("Progress 10%\rProgress 50%\rDone        ");
        assert_eq!(line.text.as_ref(), "Done");
        assert_eq!(ConsoleLine::parse("abc\u{8}d").text.as_ref(), "abd");
        assert_eq!(ConsoleLine::parse("a\tb").text.as_ref(), "a   b");
        assert_eq!(ConsoleLine::parse("line\r").text.as_ref(), "line");
    }

    #[test]
    fn colours_map_to_the_themes_hues() {
        let line = ConsoleLine::parse(
            "\u{1b}[38;5;196mred\u{1b}[38;2;80;120;255mblue\u{1b}[48;5;2m\u{1b}[39mplain",
        );
        assert_eq!(line.text.as_ref(), "redblueplain");
        let hues: Vec<_> = styled(&line)
            .iter()
            .map(|(text, style)| (*text, style.hue))
            .collect();
        assert_eq!(hues, [("red", Some(Hue::Red)), ("blue", Some(Hue::Blue))]);
    }

    #[test]
    fn uncoloured_compiler_labels_are_highlighted() {
        let line = ConsoleLine::parse("error[E0308]: mismatched types");
        assert_eq!(styled(&line)[0].0, "error");
        assert_eq!(styled(&line)[0].1.hue, Some(Hue::Red));
        assert_eq!(styled(&ConsoleLine::parse("  warning: unused")).len(), 1);
        assert!(ConsoleLine::parse("errors: none").runs.is_empty());
    }
}
