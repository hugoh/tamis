use std::io::Write;

use anyhow::Result;

use super::super::App;
use super::super::style::{BOLD, Cell, DIM, GREEN, MAGENTA, RED, Style, YELLOW, cell, write_table};
use super::common::{local, open, palette};
use crate::store::Decision;

pub(in super::super) fn log(app: &mut App, number: i64) -> Result<()> {
    let (_, store) = open(app)?;
    let decisions = store.decisions(number)?;
    write_decisions(app, &decisions)
}

fn verdict_style(verdict: &str) -> Style {
    match verdict {
        "spam" => RED,
        "uncertain" => YELLOW,
        "good" => GREEN,
        _ => "",
    }
}

fn write_decisions(app: &mut App, decisions: &[Decision]) -> Result<()> {
    if decisions.is_empty() {
        writeln!(app.env.stdout, "No decisions yet.")?;
        return Ok(());
    }
    let colors = palette(app)?;

    let mut rows: Vec<Vec<Cell>> = vec![
        [
            "ID", "TIME", "VERDICT", "REASON", "SCORE", "ACTION", "FOLDER", "FROM", "SUBJECT",
        ]
        .iter()
        .map(|heading| cell(heading, BOLD))
        .collect(),
    ];
    for decision in decisions {
        let style = verdict_style(&decision.verdict);
        rows.push(vec![
            cell(&decision.id.to_string(), ""),
            cell(&local(decision.at), DIM),
            cell(&decision.verdict, style),
            cell(&decision.reason, DIM),
            cell(&format!("{:.2}", decision.score), style),
            action_cell(decision),
            cell(&decision.mailbox, ""),
            cell(&decision.from, ""),
            cell(&decision.subject, ""),
        ]);
    }
    write_table(&mut app.env.stdout, colors, &rows)
}

/// Shows what was done; what live mode would do, and nothing, are dimmed, and
/// a later undo or correction stands out.
fn action_cell(decision: &Decision) -> Cell {
    let mut action = if decision.action == "none" {
        cell(&decision.action, DIM)
    } else if !decision.applied {
        cell(&format!("would {}", decision.action), DIM)
    } else {
        cell(&decision.action, "")
    };

    if decision.undone {
        action.push((" (undone)".to_owned(), MAGENTA));
    } else if decision.corrected_by > 0 {
        action.push((
            format!(" (corrected by {})", decision.corrected_by),
            MAGENTA,
        ));
    }
    action
}
