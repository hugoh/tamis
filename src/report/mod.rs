//! What the health and probe reports share.

use std::io::{self, Write};

use serde::Serialize;

const COLUMN_PADDING: usize = 2;

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
/// shortcut: one width per column for the whole table, where Go's tabwriter
/// restarts it after a row that lacks the column; upgrade if a report mixes
/// row shapes.
///
/// # Errors
/// Fails when `out` can't be written.
pub fn write_table<R: AsRef<[String]>>(out: &mut impl Write, rows: &[R]) -> io::Result<()> {
    let mut widths: Vec<usize> = Vec::new();
    for row in rows {
        let cells = row.as_ref();
        for (column, cell) in cells.iter().take(cells.len().saturating_sub(1)).enumerate() {
            if widths.len() <= column {
                widths.push(0);
            }
            widths[column] = widths[column].max(cell.chars().count());
        }
    }

    for row in rows {
        let cells = row.as_ref();
        let mut line = String::new();
        for (column, cell) in cells.iter().enumerate() {
            line.push_str(cell);
            if column + 1 < cells.len() {
                let padding = widths[column] + COLUMN_PADDING - cell.chars().count();
                line.push_str(&" ".repeat(padding));
            }
        }
        writeln!(out, "{line}")?;
    }
    Ok(())
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
