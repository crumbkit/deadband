//! Draws the reclaims of `deadband top`: the section under the job rows,
//! the line above the rule that names a running reclaim's plan or a
//! reclaimer's failures, and the words the reason line adds.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::style::Style;
use ratatui::text::Line;

use super::row::{Columns, names};
use super::{DIM, GREEN, RED, TEAL, TEXT, bold, fg, fit, put, short};
use crate::protocol::ReclaimPlan;
use crate::reclaim::size_bytes;
use crate::tui::model::{Ending, Model, Reclaim, ReclaimRow, Row, Standing, Status};

/// The fewest job lines that the reclaim section leaves free. A terminal
/// with fewer lines leaves the section out.
pub(super) const MIN_JOBS: usize = 3;

/// The lines that the section takes for `rows`: a blank line, its label
/// and one line for each reclaimer, or none for no row.
pub(super) const fn height(rows: &[ReclaimRow]) -> usize {
    if rows.is_empty() { 0 } else { rows.len() + 2 }
}

/// Returns `bytes` in the largest unit that holds at least one of it, with
/// one decimal below 10 and none from there. `long` spells the units as
/// `GiB`, and otherwise they are `G`.
fn size(bytes: u64, long: bool) -> String {
    const UNITS: [(&str, &str); 4] = [("K", "KiB"), ("M", "MiB"), ("G", "GiB"), ("T", "TiB")];
    let mut chosen = None;
    for (power, unit) in (1_u32..).zip(UNITS) {
        let scale = 1024_u64.pow(power);
        if bytes >= scale {
            chosen = Some((scale, unit));
        }
    }
    let Some((scale, (short_unit, long_unit))) = chosen else {
        return if long {
            format!("{bytes} B")
        } else {
            bytes.to_string()
        };
    };
    let tenths = u128::from(bytes) * 10 / u128::from(scale);
    let number = if tenths >= 100 || tenths % 10 == 0 {
        (tenths / 10).to_string()
    } else {
        format!("{}.{}", tenths / 10, tenths % 10)
    };
    if long {
        format!("{number} {long_unit}")
    } else {
        format!("{number}{short_unit}")
    }
}

/// Draws the reclaims section from line `y`: a blank line, the label, and
/// one row for each reclaimer, on an axis `width` columns wide.
pub(super) fn draw_section(
    buffer: &mut Buffer,
    y: u16,
    rows: &[ReclaimRow],
    width: u16,
    columns: Columns,
) {
    put(buffer, 1, y + 1, "reclaims", fg(DIM));
    for (y, row) in (y + 2..).zip(rows) {
        draw_row(buffer, y, row, width, columns);
    }
}

/// Draws one reclaimer's row.
fn draw_row(buffer: &mut Buffer, y: u16, row: &ReclaimRow, width: u16, columns: Columns) {
    let live = matches!(row.standing, Standing::Running(_));
    let names_row = Row {
        workspace: row.command.clone(),
        label: row.name.clone(),
        shows_label: true,
        short: row.need.clone(),
        fast: false,
        waited: 0..0,
        ran: 0..0,
        mark: None,
        status: Status::Waiting(Duration::ZERO),
        stuck: Vec::new(),
    };
    names(buffer, y, &names_row, columns, live);
    let axis_x = columns.axis_x();
    for bar in &row.bars {
        for column in bar.columns.clone() {
            put(buffer, axis_x + column, y, "━", fg(TEAL));
        }
        if let Some((column, ending)) = bar.end {
            let (sign, color) = match ending {
                Ending::Failed => ("✗", RED),
                Ending::Done | Ending::Open => ("✓", GREEN),
            };
            put(buffer, axis_x + column, y, sign, bold(color));
        }
    }
    let (text, style) = standing(row.standing);
    put(buffer, axis_x + width + 2, y, &text, style);
}

/// Returns the text at the right of a reclaimer's row and its style.
fn standing(standing: Standing) -> (String, Style) {
    match standing {
        Standing::Idle => ("idle".to_owned(), fg(DIM)),
        Standing::Running(age) => (format!("run {}", short(age)), fg(TEAL)),
        Standing::Failed(ago) => (format!("failed {}", short(ago)), bold(RED)),
        Standing::Freed { bytes: None, ago } => (format!("done {}", short(ago)), fg(DIM)),
        Standing::Freed {
            bytes: Some(bytes),
            ago,
        } => {
            let color = if bytes > 0 { GREEN } else { DIM };
            (
                format!("freed {} {}", size(bytes, false), short(ago)),
                fg(color),
            )
        }
    }
}

/// Returns the line above the rule for the reclaims, as styled pieces:
/// the plan of the reclaim that runs, or a reclaimer's failures.
pub(super) fn note(model: &Model, now: Instant) -> Option<Vec<(String, Style)>> {
    match model.reclaim(now) {
        Reclaim::Quiet => None,
        Reclaim::Running { .. } => model.plan().map(plan),
        Reclaim::Failed {
            command,
            times,
            why,
        } => {
            let count = if times == 1 {
                "once".to_owned()
            } else {
                format!("{times} times")
            };
            Some(vec![
                (format!("{command} failed {count}: "), bold(RED)),
                (why, fg(TEXT)),
            ])
        }
    }
}

/// Returns the pieces of the line that says what `plan` does.
fn plan(plan: &ReclaimPlan) -> Vec<(String, Style)> {
    let names = |names: &[String]| names.join(", ");
    let stop: Vec<String> = plan
        .stop
        .iter()
        .map(|name| name.as_str().to_owned())
        .collect();
    let mut stops = if stop.is_empty() {
        "stop nothing".to_owned()
    } else {
        format!("stop {}", names(&stop))
    };
    if plan.bytes > 0 {
        stops.push_str(&format!(" ({})", size(plan.bytes, true)));
    }
    let mut pieces = vec![
        (format!("{} plans: ", plan.command.as_str()), fg(DIM)),
        (stops, fg(TEXT)),
    ];
    let mut kept = String::new();
    let mut group: Vec<String> = Vec::new();
    let mut why = "";
    let close = |group: &mut Vec<String>, why: &str, kept: &mut String| {
        if group.is_empty() {
            return;
        }
        kept.push_str(&format!("; keep {}", group.join(", ")));
        if !why.is_empty() {
            kept.push_str(&format!(" ({why})"));
        }
        group.clear();
    };
    for item in &plan.keep {
        if item.why.as_str() != why {
            close(&mut group, why, &mut kept);
            why = item.why.as_str();
        }
        group.push(item.name.as_str().to_owned());
    }
    close(&mut group, why, &mut kept);
    if plan.more > 0 {
        kept.push_str(&format!("; and {} more", plan.more));
    }
    if !kept.is_empty() {
        pieces.push((kept, fg(TEAL)));
    }
    pieces
}

/// Draws `pieces` from column `x` of line `y`, cut to `room` columns.
pub(super) fn put_pieces(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    pieces: &[(String, Style)],
    room: usize,
) {
    let whole: String = pieces.iter().map(|(text, _)| text.as_str()).collect();
    let shown = fit(&whole, room);
    let mut rest = shown.as_str();
    let mut at = x;
    for (text, style) in pieces {
        let take: String = rest.chars().take(text.chars().count()).collect();
        put(buffer, at, y, &take, *style);
        let width = u16::try_from(Line::from(take.as_str()).width()).unwrap_or(0);
        at = at.saturating_add(width);
        rest = rest.get(take.len()..).unwrap_or("");
    }
}

/// Returns what the reason line adds after the hold: which reclaimer frees
/// how much, or when the next reclaim starts.
pub(super) fn reason_suffix(model: &Model, now: Instant) -> Option<String> {
    if let Reclaim::Running {
        command,
        need,
        since,
    } = model.reclaim(now)
    {
        let freeing = need
            .split_once('=')
            .and_then(|(_, amount)| size_bytes(amount))
            .map_or(need, |bytes| size(bytes, true));
        return Some(format!(
            "{command} is freeing {freeing}, started {} ago",
            short(since)
        ));
    }
    let next = model.next_reclaim(now)?;
    Some(if next < Duration::from_secs(1) {
        "the next reclaim starts now".to_owned()
    } else {
        format!("the next reclaim starts in {}", short(next))
    })
}
