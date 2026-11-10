//! Draws one job's row: its workspace and job columns at the left, its bars
//! on the time axis, and its state at the right.

use ratatui::buffer::Buffer;
use ratatui::style::Color;

use super::{AMBER, DIM, GREEN, HI, RED, TEAL, TEXT, VIOLET, bold, fg, fit, put, short};
use crate::protocol::Exit;
use crate::tui::model::{Row, Status};

/// The terminal width from which the left columns take their wide sizes.
const WIDE: u16 = 100;

/// The widths of the workspace and job columns at the left of the rows,
/// which depend on the terminal's width. One blank column lies between
/// them, the fast-lane sign follows the job column, and the time axis
/// starts three columns after the job column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Columns {
    /// The width of the workspace column.
    workspace: u16,
    /// The width of the job column.
    job: u16,
}

impl Columns {
    /// Returns the columns of a terminal `width` columns wide: 16 and 26 at
    /// 100 columns or more, and 12 and 16 below that.
    pub(super) const fn of(width: u16) -> Self {
        if width >= WIDE {
            Self {
                workspace: 16,
                job: 26,
            }
        } else {
            Self {
                workspace: 12,
                job: 16,
            }
        }
    }

    /// Returns the column where the job column starts.
    pub(super) const fn job_x(self) -> u16 {
        1 + self.workspace + 1
    }

    /// Returns the column of the fast-lane sign, right after the job column.
    const fn lane_x(self) -> u16 {
        self.job_x() + self.job
    }

    /// Returns the column where the time axis starts.
    pub(super) const fn axis_x(self) -> u16 {
        self.lane_x() + 3
    }
}

/// Draws the job's workspace, then its label and short command in the job
/// column. The label is bold while the job waits or runs, and the label is
/// left out when the row says so.
fn names(buffer: &mut Buffer, y: u16, row: &Row, columns: Columns, live: bool) {
    let (workspace, label, command) = if live {
        (fg(TEAL), bold(HI), fg(TEXT))
    } else {
        (fg(DIM), fg(DIM), fg(DIM))
    };
    let workspace_width = usize::from(columns.workspace);
    put(
        buffer,
        1,
        y,
        &fit(&row.workspace, workspace_width),
        workspace,
    );
    let end = columns.lane_x();
    let mut x = columns.job_x();
    if row.shows_label {
        let text = fit(&row.label, usize::from(columns.job));
        put(buffer, x, y, &text, label);
        let width = u16::try_from(ratatui::text::Line::from(text.as_str()).width()).unwrap_or(end);
        x = x.saturating_add(width).saturating_add(2);
    }
    if !row.short.is_empty() && x < end {
        put(
            buffer,
            x,
            y,
            &fit(&row.short, usize::from(end - x)),
            command,
        );
    }
}

/// Draws one job's row on an axis `width` columns wide.
pub(super) fn draw_row(buffer: &mut Buffer, y: u16, row: &Row, width: u16, columns: Columns) {
    let live = matches!(
        row.status,
        Status::Waiting(_) | Status::Running(_) | Status::Stuck(_)
    );
    names(buffer, y, row, columns, live);
    let axis_x = columns.axis_x();
    let lane = if row.fast {
        put(buffer, columns.lane_x(), y, "▲", fg(VIOLET));
        VIOLET
    } else {
        TEAL
    };
    for column in row.waited.clone() {
        put(buffer, axis_x + column, y, "░", fg(AMBER));
    }
    let glyph = if live { "█" } else { "▒" };
    for column in row.ran.clone() {
        put(buffer, axis_x + column, y, glyph, fg(lane));
    }
    for column in row.stuck.iter().flat_map(Clone::clone) {
        put(buffer, axis_x + column, y, "▓", fg(RED));
    }
    let (text, color, sign) = status_text(row.status);
    if let (Some(column), Some((sign, sign_color))) = (row.mark, sign) {
        put(buffer, axis_x + column, y, sign, bold(sign_color));
    }
    let style = if matches!(row.status, Status::Stuck(_)) {
        bold(color)
    } else {
        fg(color)
    };
    put(buffer, axis_x + width + 2, y, &text, style);
}

/// Returns the text at the right of a row, its colour, and the sign after
/// the end of a job that has ended with the sign's colour.
fn status_text(status: Status) -> (String, Color, Option<(&'static str, Color)>) {
    match status {
        Status::Waiting(age) => (format!("wait {}", short(age)), AMBER, None),
        Status::Running(age) => (format!("run {}", short(age)), TEAL, None),
        Status::Stuck(quiet) => (format!("stuck {}", short(quiet)), RED, None),
        Status::Left { waited } => (format!("left {}", short(waited)), DIM, Some(("·", DIM))),
        Status::Finished { exit, ran } => {
            let (words, color, sign) = match exit {
                Exit::Exited { code: 0 } => ("ok".to_owned(), DIM, ("✓", GREEN)),
                Exit::Exited { code } => (format!("exit {code}"), RED, ("✗", RED)),
                Exit::Signalled { signal } => (format!("signal {signal}"), RED, ("✗", RED)),
                Exit::Lost => ("lost".to_owned(), AMBER, ("?", AMBER)),
            };
            (format!("{words} {}", short(ran)), color, Some(sign))
        }
    }
}
