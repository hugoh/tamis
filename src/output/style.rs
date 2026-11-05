//! Colour and aligned tables for terminal output.

use std::io::Write;

use anyhow::{Context as _, Result, bail};
use unicode_width::UnicodeWidthStr as _;

use anstyle::AnsiColor;
pub use anstyle::Style;

pub const PLAIN: Style = Style::new();
pub const BOLD: Style = Style::new().bold();
pub const DIM: Style = Style::new().dimmed();
pub const RED: Style = AnsiColor::Red.on_default();
pub const GREEN: Style = AnsiColor::Green.on_default();
pub const YELLOW: Style = AnsiColor::Yellow.on_default();
pub const BLUE: Style = AnsiColor::Blue.on_default();
pub const MAGENTA: Style = AnsiColor::Magenta.on_default();
pub const BOLD_RED: Style = RED.bold();
pub const BOLD_YELLOW: Style = YELLOW.bold();

/// Paints text, or leaves it alone when colour is off.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    on: bool,
}

impl Palette {
    /// Decides whether to colour output from `--color`: always and never are
    /// explicit; auto colours a terminal unless `NO_COLOR` is set
    /// (no-color.org) or `TERM` is dumb.
    ///
    /// # Errors
    /// When `mode` is not auto, always or never.
    pub fn new(
        mode: &str,
        is_terminal: bool,
        getenv: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        let on = match mode {
            "always" => true,
            "never" => false,
            "auto" | "" => {
                is_terminal
                    && getenv("NO_COLOR").is_none_or(|v| v.is_empty())
                    && getenv("TERM").as_deref() != Some("dumb")
            }
            other => bail!("--color must be auto, always or never, not {other:?}"),
        };
        Ok(Self { on })
    }

    #[must_use]
    pub const fn off() -> Self {
        Self { on: false }
    }

    #[must_use]
    pub fn paint(self, text: &str, style: Style) -> String {
        if !self.on || style == PLAIN || text.is_empty() {
            text.to_owned()
        } else {
            format!("{style}{text}{style:#}")
        }
    }
}

/// A run of text in one style; a cell is one or more spans.
pub type Span = (String, Style);
pub type Cell = Vec<Span>;

#[must_use]
pub fn cell(text: &str, style: Style) -> Cell {
    vec![(text.to_owned(), style)]
}

fn width(cell: &Cell) -> usize {
    cell.iter().map(|(text, _)| text.width()).sum()
}

const TABLE_GAP: usize = 2;

/// Prints rows as aligned columns. Columns are padded by the width of their
/// visible text, so colour doesn't shift them; the last cell of a row isn't
/// padded, so it doesn't count towards the width either (rows may differ in
/// length, as a heading above a table does).
///
/// # Errors
/// When `out` can't be written.
pub fn write_table(out: &mut dyn Write, colors: Palette, rows: &[Vec<Cell>]) -> Result<()> {
    let mut widths: Vec<usize> = Vec::new();
    for row in rows {
        for (index, cell) in row.iter().take(row.len().saturating_sub(1)).enumerate() {
            if widths.len() == index {
                widths.push(0);
            }
            widths[index] = widths[index].max(width(cell));
        }
    }

    let mut text = String::new();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            for (part, style) in cell {
                text.push_str(&colors.paint(part, *style));
            }
            if index + 1 < row.len() {
                text.push_str(&" ".repeat(widths[index] - width(cell) + TABLE_GAP));
            }
        }
        text.push('\n');
    }
    out.write_all(text.as_bytes()).context("write table")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn colour_follows_the_mode_the_terminal_and_the_environment() {
        let cases = [
            ("always", false, &[][..], true),
            ("never", true, &[][..], false),
            ("auto", true, &[][..], true),
            ("auto", false, &[][..], false),
            ("auto", true, &[("NO_COLOR", "1")][..], false),
            ("auto", true, &[("TERM", "dumb")][..], false),
            ("", true, &[][..], true),
        ];
        for (mode, terminal, vars, want) in cases {
            let palette = Palette::new(mode, terminal, &env(vars)).unwrap();
            assert_eq!(
                palette.paint("x", RED) != "x",
                want,
                "{mode} {terminal} {vars:?}"
            );
        }
        assert!(Palette::new("sometimes", true, &env(&[])).is_err());
    }

    #[test]
    fn painting_leaves_empty_text_and_styles_alone() {
        let on = Palette::new("always", false, &env(&[])).unwrap();

        assert_eq!(on.paint("hi", RED), "\x1b[31mhi\x1b[0m");
        assert_eq!(on.paint("", RED), "");
        assert_eq!(on.paint("hi", PLAIN), "hi");
    }

    #[test]
    fn tables_align_by_visible_width_whatever_the_colour() {
        let rows = vec![
            vec![cell("a", BOLD), cell("long", PLAIN), cell("tail", DIM)],
            vec![cell("bbbb", PLAIN), cell("x", PLAIN)],
            vec![cell("heading", BOLD)],
        ];
        for mode in ["never", "always"] {
            let palette = Palette::new(mode, false, &env(&[])).unwrap();
            let mut out = Vec::new();
            write_table(&mut out, palette, &rows).unwrap();

            let plain: String = String::from_utf8(out)
                .unwrap()
                .split('\x1b')
                .map(|part| part.split_once('m').map_or(part, |(_, rest)| rest))
                .collect();
            assert_eq!(plain, "a     long  tail\nbbbb  x\nheading\n", "{mode}");
        }
    }

    #[test]
    fn tables_align_wide_characters_by_the_columns_they_take() {
        let rows = vec![
            vec![cell("日本", PLAIN), cell("x", PLAIN)],
            vec![cell("ab", PLAIN), cell("y", PLAIN)],
        ];
        let off = Palette::new("never", false, &env(&[])).unwrap();
        let mut out = Vec::new();
        write_table(&mut out, off, &rows).unwrap();

        assert_eq!(String::from_utf8(out).unwrap(), "日本  x\nab    y\n");
    }
}
