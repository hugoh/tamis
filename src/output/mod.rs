//! Output formatting shared by the CLI and the tools: JSON, aligned tables and
//! colour.

pub mod style;

use std::io::{self, Write};

use serde::Serialize;

use style::{Cell, PLAIN, Palette, cell};

/// Renders `value` as indented JSON followed by a newline.
///
/// # Errors
/// Fails when `value` can't be serialized or `out` can't be written.
pub fn write_json(out: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)
}

/// Lays out `rows` in aligned columns: every cell but a row's last is padded
/// to the widest in its column plus two spaces.
///
/// # Errors
/// Fails when `out` can't be written.
pub fn write_table<R: AsRef<[String]>>(out: &mut impl Write, rows: &[R]) -> io::Result<()> {
    let rows: Vec<Vec<Cell>> = rows
        .iter()
        .map(|row| row.as_ref().iter().map(|text| cell(text, PLAIN)).collect())
        .collect();
    out.write_all(style::render_table(Palette::off(), &rows).as_bytes())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn row(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|c| (*c).to_owned()).collect()
    }

    #[test]
    fn json_is_indented_with_a_newline() {
        let mut out = Vec::new();
        write_json(&mut out, &BTreeMap::from([("a", 1)])).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "{\n  \"a\": 1\n}\n");
    }

    #[test]
    fn a_failed_table_write_keeps_its_error_kind() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let err = write_table(&mut Closed, &[row(&["a"])]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn tables_align_their_columns() {
        let cases = [
            (
                vec![row(&["a", "b"]), row(&["longer", "c"])],
                "a       b\nlonger  c\n",
            ),
            (
                vec![row(&["é", "x", "tail"]), row(&["ab", "yy", "z"])],
                "é   x   tail\nab  yy  z\n",
            ),
            (
                vec![row(&["日本", "x"]), row(&["ab", "y"])],
                "日本  x\nab    y\n",
            ),
            (vec![row(&["only"])], "only\n"),
            (vec![], ""),
        ];
        for (rows, want) in cases {
            let mut out = Vec::new();
            write_table(&mut out, &rows).unwrap();
            assert_eq!(String::from_utf8(out).unwrap(), want);
        }
    }
}
