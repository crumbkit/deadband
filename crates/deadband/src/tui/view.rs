//! Draws the model of `deadband top` into a ratatui buffer: rows above, the
//! load and what held jobs below, on one time axis. It parses no text and
//! writes only what the model's types hold.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

use super::model::{Absence, Axis, Gate, Model, Phase, Row, Status};
use crate::protocol::{Exit, Label, OutsideJobs};

mod graph;
#[cfg(test)]
mod tests;

const BG: Color = Color::Rgb(0x0d, 0x15, 0x19);
const LINE: Color = Color::Rgb(0x2b, 0x3d, 0x45);
const FAINT: Color = Color::Rgb(0x1a, 0x28, 0x2e);
const DIM: Color = Color::Rgb(0x7f, 0x95, 0x9e);
const TEXT: Color = Color::Rgb(0xc9, 0xd6, 0xdb);
const HI: Color = Color::Rgb(0xf1, 0xf7, 0xf9);
const TEAL: Color = Color::Rgb(0x3a, 0xa6, 0xc9);
const GREEN: Color = Color::Rgb(0x4f, 0xd0, 0x8b);
const AMBER: Color = Color::Rgb(0xf2, 0xb8, 0x4b);
const RED: Color = Color::Rgb(0xf4, 0x66, 0x5f);
const VIOLET: Color = Color::Rgb(0xa8, 0x97, 0xf5);
const INK: Color = Color::Rgb(0x06, 0x18, 0x1f);

/// The column where the time axis starts.
const AXIS_X: u16 = 24;
/// The columns right of the time axis: two for the end marks and the gap,
/// and fourteen for the status. The longest status is `signal 64 999h`. A
/// job that ran over 999 hours, or a client that reports a code or signal
/// outside what the system can give, loses the end of its text at the edge.
const RIGHT: u16 = 16;
/// The widest label. The column after it holds the fast-lane sign.
const LABEL_WIDTH: usize = 20;
/// The first line of the rows.
const ROWS_Y: u16 = 3;
/// The lines below the rows: the rule, the graph, the strip, the legend,
/// the reason and the keys.
const BOTTOM: u16 = 9;
/// The minutes that the axis labels.
const MINUTES: [u64; 5] = [10, 8, 6, 4, 2];

/// Writes `text` at `(x, y)`, clipped to the buffer.
fn put(buffer: &mut Buffer, x: u16, y: u16, text: &str, style: Style) {
    let area = buffer.area;
    if x < area.width && y < area.height {
        buffer.set_stringn(x, y, text, usize::from(area.width - x), style);
    }
}

/// Returns a style with this foreground colour.
fn fg(color: Color) -> Style {
    Style::default().fg(color)
}

/// Returns a bold style with this foreground colour.
fn bold(color: Color) -> Style {
    fg(color).add_modifier(Modifier::BOLD)
}

/// Returns `text` cut to `width` columns, ending in an ellipsis when cut.
fn fit(text: &str, width: usize) -> String {
    if Line::from(text).width() <= width {
        return text.to_owned();
    }
    let mut kept = text.to_owned();
    while Line::from(kept.as_str()).width() >= width {
        kept.pop();
    }
    kept.push('…');
    kept
}

/// Returns `duration` in its largest whole unit.
fn short(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}

/// Draws `model` at `now` into `buffer`.
pub(super) fn draw(buffer: &mut Buffer, model: &Model, now: Instant) {
    let (columns, lines) = (buffer.area.width, buffer.area.height);
    let area = buffer.area;
    buffer.set_style(area, Style::default().bg(BG));
    let phase = model.phase(now, columns, lines);
    if let Phase::TooSmall { columns, lines } = phase {
        too_small(buffer, columns, lines);
        return;
    }
    let width = columns - AXIS_X - RIGHT;
    let axis = Axis::new(width);
    let rows_end = lines - BOTTOM;
    let note = activity_note(model);
    let capacity = usize::from(rows_end - ROWS_Y) - usize::from(note.is_some());
    let rows = model.rows(now, axis, capacity);
    header(buffer, model, &phase, rows.unseen);
    axis_labels(buffer, axis, width);
    if let Phase::DaemonDown(absence) = phase {
        no_daemon(buffer, absence, rows_end);
    } else {
        grid(buffer, axis, rows_end);
        for (y, row) in (ROWS_Y..rows_end).zip(&rows.rows) {
            draw_row(buffer, y, row, width);
        }
        if let Some((text, style)) = note {
            put(
                buffer,
                1,
                rows_end - 1,
                &fit(&text, usize::from(columns - 2)),
                style,
            );
        }
    }
    put(
        buffer,
        0,
        rows_end,
        &"─".repeat(usize::from(columns)),
        fg(LINE),
    );
    let no_activity = rows.rows.iter().any(|row| !row.stuck.is_empty());
    graph::draw(
        buffer,
        model,
        now,
        rows_end + 1,
        (AXIS_X, width),
        no_activity,
    );
    reason(buffer, model, &phase, lines - 2);
    put(buffer, 1, lines - 1, "q", bold(TEAL));
    put(buffer, 3, lines - 1, "quit", fg(DIM));
}

/// Returns the line above the rule and its style: the count of stuck jobs
/// with the busiest process outside the jobs, or that activity is unknown.
fn activity_note(model: &Model) -> Option<(String, Style)> {
    let count = model.stuck_count();
    if count > 0 {
        let jobs = if count == 1 {
            "1 job shows no activity".to_owned()
        } else {
            format!("{count} jobs show no activity")
        };
        let hint = match model.busiest() {
            Some(OutsideJobs::Busy { name, cpu_percent }) => {
                let clean = OutsideJobs::Busy {
                    name: Label::new(name).as_str().to_owned(),
                    cpu_percent: *cpu_percent,
                };
                format!(" · {clean}")
            }
            Some(OutsideJobs::Quiet) | None => String::new(),
        };
        return Some((format!("{jobs}{hint}"), bold(RED)));
    }
    model.unreadable().then(|| {
        (
            "activity unknown: deadband could not read the jobs' processes".to_owned(),
            fg(DIM),
        )
    })
}

/// Says what size the terminal has and what the view needs.
fn too_small(buffer: &mut Buffer, columns: u16, lines: u16) {
    let size = format!("This terminal is {columns} x {lines}.");
    put(buffer, 1, 1, &size, bold(HI));
    let needs = "deadband top needs at least 80 x 16. Widen it, or press q.";
    put(buffer, 1, 2, needs, fg(TEXT));
}

/// Returns the pill's text and colour.
fn pill(model: &Model, phase: &Phase) -> (String, Color) {
    match phase {
        Phase::DaemonDown(Absence::NotRunning) => (" NO DAEMON ".to_owned(), DIM),
        Phase::DaemonDown(Absence::MaybeOlder) => (" OLDER DAEMON ".to_owned(), AMBER),
        Phase::Connecting(_) => (" CONNECTING ".to_owned(), DIM),
        Phase::Live | Phase::Stale { .. } | Phase::TooSmall { .. } => match model.held() {
            None => (" OPEN ".to_owned(), GREEN),
            Some(hold) => {
                let (name, color) = gate_name(Gate::from(Some(hold)));
                (format!(" HELD · {} ", name.to_uppercase()), color)
            }
        },
    }
}

/// Returns the name of what holds jobs, and the colour of the pill.
const fn gate_name(gate: Gate) -> (&'static str, Color) {
    match gate {
        Gate::Open => ("open", GREEN),
        Gate::Full => ("full", AMBER),
        Gate::Load => ("load", AMBER),
        Gate::Memory => ("memory", VIOLET),
        Gate::Disk => ("disk", RED),
    }
}

/// Returns the words after the title in the header.
fn subtitle(model: &Model, phase: &Phase, unseen: usize) -> String {
    let mut words = "last 10 minutes".to_owned();
    if let Some(up) = model.restarted() {
        let restarted = format!(
            " · daemon restarted {} ago, earlier jobs are gone",
            short(up)
        );
        words.push_str(&restarted);
    }
    match phase {
        Phase::Stale { silent, why } => {
            words.push_str(&format!(" · no answer for {}: {why}", short(*silent)));
        }
        Phase::Connecting(Some(why)) => words.push_str(&format!(" · {why}")),
        Phase::Connecting(None) | Phase::Live | Phase::DaemonDown(_) | Phase::TooSmall { .. } => {}
    }
    if unseen > 0 {
        let jobs = if unseen == 1 { "job" } else { "jobs" };
        words.push_str(&format!(" · {unseen} more {jobs} not shown"));
    }
    words
}

/// Draws the first line: the title, what the view knows, and the pill.
fn header(buffer: &mut Buffer, model: &Model, phase: &Phase, unseen: usize) {
    put(buffer, 0, 0, " deadband ", bold(INK).bg(TEAL));
    let (text, color) = pill(model, phase);
    let pill_x = buffer.area.width - 2 - u16::try_from(text.chars().count()).unwrap_or(0);
    let count = model.stuck_count();
    let stuck = (count > 0).then(|| format!(" {count} STUCK "));
    let stuck_width = stuck.as_ref().map_or(0, |label| {
        u16::try_from(label.chars().count()).unwrap_or(0) + 1
    });
    if let Some(label) = &stuck {
        put(buffer, pill_x - stuck_width, 0, label, bold(INK).bg(RED));
    }
    let room = usize::from(pill_x.saturating_sub(12 + stuck_width));
    put(
        buffer,
        11,
        0,
        &fit(&subtitle(model, phase, unseen), room),
        fg(DIM),
    );
    put(buffer, pill_x, 0, &text, bold(INK).bg(color));
}

/// Returns the screen column of something `minutes` old on `axis`.
fn minute_x(axis: Axis, minutes: u64) -> u16 {
    let age = Duration::from_secs(minutes * 60);
    u16::try_from(i64::from(AXIS_X) + axis.column(age)).unwrap_or(AXIS_X)
}

/// Draws the line that names the columns and the minutes.
fn axis_labels(buffer: &mut Buffer, axis: Axis, width: u16) {
    put(buffer, 1, 2, "job", fg(DIM));
    for minutes in MINUTES {
        let label = format!("-{minutes}m");
        put(buffer, minute_x(axis, minutes), 2, &label, fg(DIM));
    }
    put(buffer, AXIS_X + width - 3, 2, "now", fg(DIM));
    put(buffer, AXIS_X + width + 2, 2, "state", fg(DIM));
}

/// Draws the faint line at each labelled minute.
fn grid(buffer: &mut Buffer, axis: Axis, rows_end: u16) {
    for minutes in MINUTES {
        let x = minute_x(axis, minutes);
        for y in ROWS_Y..rows_end {
            put(buffer, x, y, "│", fg(FAINT));
        }
    }
}

/// Draws one job's row.
fn draw_row(buffer: &mut Buffer, y: u16, row: &Row, width: u16) {
    let live = matches!(
        row.status,
        Status::Waiting(_) | Status::Running(_) | Status::Stuck(_)
    );
    let label_style = if live { bold(HI) } else { fg(DIM) };
    put(buffer, 1, y, &fit(&row.label, LABEL_WIDTH), label_style);
    let lane = if row.fast {
        put(buffer, 21, y, "▲", fg(VIOLET));
        VIOLET
    } else {
        TEAL
    };
    for column in row.waited.clone() {
        put(buffer, AXIS_X + column, y, "░", fg(AMBER));
    }
    let glyph = if live { "█" } else { "▒" };
    for column in row.ran.clone() {
        put(buffer, AXIS_X + column, y, glyph, fg(lane));
    }
    for column in row.stuck.iter().flat_map(Clone::clone) {
        put(buffer, AXIS_X + column, y, "▓", fg(RED));
    }
    let (text, color, sign) = status_text(row.status);
    if let (Some(column), Some((sign, sign_color))) = (row.mark, sign) {
        put(buffer, AXIS_X + column, y, sign, bold(sign_color));
    }
    let style = if matches!(row.status, Status::Stuck(_)) {
        bold(color)
    } else {
        fg(color)
    };
    put(buffer, AXIS_X + width + 2, y, &text, style);
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

/// Draws why no timeline can show, centred in the rows' area. A short
/// area drops the blank lines.
fn no_daemon(buffer: &mut Buffer, absence: Absence, rows_end: u16) {
    let daemon = super::daemon_call();
    let (first, second, third) = match absence {
        Absence::NotRunning => (
            "The deadband daemon is not running.",
            "Jobs start at once while it is down.",
            format!("Start it with: {daemon}"),
        ),
        Absence::MaybeOlder => (
            "The daemon closed the connection without answering.",
            "The daemon may be older than this command.",
            format!("Stop the daemon that runs now, then run: {daemon}"),
        ),
    };
    let area = rows_end - ROWS_Y;
    let parts = [
        Some((first, bold(HI))),
        Some((second, fg(TEXT))),
        (area >= 6).then_some(("", fg(TEXT))),
        Some((third.as_str(), fg(TEAL))),
        (area >= 6).then_some(("", fg(TEXT))),
        Some(("Retrying every second…", fg(DIM))),
    ];
    let shown: Vec<_> = parts.into_iter().flatten().collect();
    let height = u16::try_from(shown.len()).unwrap_or(area);
    let middle = buffer.area.width / 2;
    let top = ROWS_Y + area.saturating_sub(height) / 2;
    for (y, (text, style)) in (top..).zip(shown) {
        let half = u16::try_from(text.chars().count() / 2).unwrap_or(0);
        put(buffer, middle.saturating_sub(half), y, text, style);
    }
}

/// Draws the line that says why jobs wait now.
fn reason(buffer: &mut Buffer, model: &Model, phase: &Phase, y: u16) {
    let Some(hold) = model
        .held()
        .filter(|_| !matches!(phase, Phase::DaemonDown(_)))
    else {
        return;
    };
    let room = usize::from(buffer.area.width - 2);
    let text = fit(&format!("waiting because: {hold}"), room);
    put(
        buffer,
        1,
        y,
        &text,
        bold(gate_name(Gate::from(Some(hold))).1),
    );
}
