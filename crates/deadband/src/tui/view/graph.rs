//! Draws the load graph, the strip of what held jobs, and the legend.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::style::Color;

use super::{AMBER, LINE, RED, TEAL, TEXT, VIOLET, bold, fg, put};
use crate::tui::model::{Gate, Model, WINDOW};

/// The graph's height in lines.
const HEIGHT: u16 = 4;
/// The braille dots in one line of a column of the graph.
const DOTS_PER_LINE: u16 = 4;
/// The bits of a braille cell's left column, from the bottom dot up.
const LEFT_DOTS: [u32; 4] = [6, 2, 1, 0];
/// The bits of a braille cell's right column, from the bottom dot up.
const RIGHT_DOTS: [u32; 4] = [7, 5, 4, 3];
/// The colour of the strip while nothing holds a job.
const OPEN: Color = Color::Rgb(0x2a, 0x6a, 0x4c);

/// Returns the colour of a strip cell for `gate`.
const fn strip_color(gate: Gate) -> Color {
    match gate {
        Gate::Open => OPEN,
        Gate::Full => TEAL,
        Gate::Load => AMBER,
        Gate::Memory => VIOLET,
        Gate::Disk => RED,
    }
}

/// Returns the age of step `index` of `steps` across the window, in whole
/// seconds, so a step reads the sample of its own second.
fn age_of(steps: u16, index: u16) -> Duration {
    let back = u64::from(steps - 1 - index);
    let millis = back * WINDOW.as_secs() * 1000 / u64::from(steps);
    Duration::from_secs((millis + 500) / 1000)
}

/// Returns the height of `load` in dots, on a scale whose top is `top`.
fn dots(load: Option<f64>, top: u16) -> u16 {
    let dots = f64::from(HEIGHT * DOTS_PER_LINE);
    let scaled = (load.unwrap_or(0.0) / f64::from(top) * dots)
        .round()
        .clamp(0.0, dots);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is clamped to 0..=16 and rounded"
    )]
    let height = scaled as u16;
    height
}

/// Returns the bits of one column of a braille cell that is filled to
/// `fill` of its dots.
fn column_bits(positions: [u32; 4], fill: u16) -> u32 {
    positions
        .iter()
        .take(usize::from(fill))
        .fold(0, |bits, position| bits | (1 << position))
}

/// Returns the colour at `permille` of the way up the scale, from teal
/// through green and amber to red.
fn scale_color(permille: u32) -> Color {
    let stops: [[u32; 3]; 4] = [
        [0x3a, 0xa6, 0xc9],
        [0x4f, 0xd0, 0x8b],
        [0xf2, 0xb8, 0x4b],
        [0xf4, 0x66, 0x5f],
    ];
    let position = permille.min(1000) * 3;
    let stop = (position / 1000).min(2);
    let part = position - stop * 1000;
    let index = usize::try_from(stop).unwrap_or(2);
    let (low, high) = (stops.get(index), stops.get(index + 1));
    let (Some(low), Some(high)) = (low, high) else {
        return RED;
    };
    let mix =
        |a: u32, b: u32| u8::try_from((a * (1000 - part) + b * part) / 1000).unwrap_or(u8::MAX);
    Color::Rgb(
        mix(low[0], high[0]),
        mix(low[1], high[1]),
        mix(low[2], high[2]),
    )
}

/// Draws the load, the cores, the strip and the legend from line `top`.
pub(super) fn draw(buffer: &mut Buffer, model: &Model, now: Instant, top: u16, x: u16, width: u16) {
    put(buffer, 1, top, "load", fg(TEXT));
    let load = model
        .latest_load()
        .map_or_else(|| "-".to_owned(), |load| format!("{load:.1}"));
    put(buffer, 8, top, &load, bold(super::HI));
    let cores = model.cores().map(usize::from);
    if let Some(cores) = cores {
        put(buffer, 1, top + 1, "cores", fg(AMBER));
        put(buffer, 8, top + 1, &cores.to_string(), fg(AMBER));
    }
    let scale_top = cores.map_or(20, |cores| {
        (u16::try_from(cores * 14 / 10).unwrap_or(u16::MAX)).max(20)
    });
    let limit = cores.and_then(|cores| u16::try_from(cores).ok());
    graph(buffer, model, now, (top, x, width), scale_top, limit);
    strip(buffer, model, now, top + HEIGHT, x, width);
    legend(buffer, top + HEIGHT + 1, x);
}

/// Draws the braille graph of the load in `(top, x, width)`. Dots that are
/// empty on the line of the cores show the limit.
fn graph(
    buffer: &mut Buffer,
    model: &Model,
    now: Instant,
    (top, x, width): (u16, u16, u16),
    scale_top: u16,
    limit: Option<u16>,
) {
    let limit_line = limit.map(|limit| top + HEIGHT - 1 - limit * HEIGHT / scale_top);
    let steps = width * 2;
    for column in 0..width {
        let load_at = |index| model.load_at(now, age_of(steps, index));
        let left = dots(load_at(2 * column), scale_top);
        let right = dots(load_at(2 * column + 1), scale_top);
        for line in 0..HEIGHT {
            let base = (HEIGHT - 1 - line) * DOTS_PER_LINE;
            let bits = column_bits(LEFT_DOTS, left.saturating_sub(base).min(DOTS_PER_LINE))
                | column_bits(RIGHT_DOTS, right.saturating_sub(base).min(DOTS_PER_LINE));
            let y = top + line;
            if bits != 0 {
                let glyph = char::from_u32(0x2800 + bits).unwrap_or(' ');
                let permille = u32::from(2 * (HEIGHT - line) - 1) * u32::from(scale_top) * 410
                    / (u32::from(HEIGHT) * u32::from(limit.unwrap_or(scale_top)));
                put(
                    buffer,
                    x + column,
                    y,
                    &glyph.to_string(),
                    fg(scale_color(permille)),
                );
            } else if Some(y) == limit_line {
                put(buffer, x + column, y, "╌", fg(AMBER));
            }
        }
    }
}

/// Draws what held jobs at each moment. A moment with no answer, or from
/// before the view opened, shows as a dashed line.
fn strip(buffer: &mut Buffer, model: &Model, now: Instant, y: u16, x: u16, width: u16) {
    put(buffer, 1, y, "held by", fg(TEXT));
    for column in 0..width {
        match model.gate_at(now, age_of(width, column)) {
            Some(gate) => put(buffer, x + column, y, "▀", fg(strip_color(gate))),
            None => put(buffer, x + column, y, "╌", fg(LINE)),
        }
    }
}

/// Draws the key to the strip's and the bars' colours. It starts left of
/// the axis when the terminal is too narrow for it there.
fn legend(buffer: &mut Buffer, y: u16, x: u16) {
    /// The columns the legend takes, with one spare on the right.
    const WIDTH: u16 = 72;
    let x = x.min(buffer.area.width.saturating_sub(WIDTH));
    let parts = [
        ("▀", OPEN, "open", 9),
        ("▀", TEAL, "full", 9),
        ("▀", AMBER, "load", 9),
        ("▀", VIOLET, "memory", 11),
        ("▀", RED, "disk", 11),
        ("░", AMBER, "waiting", 13),
        ("█", TEAL, "running", 0),
    ];
    let mut at = x;
    for (sign, color, name, width) in parts {
        put(buffer, at, y, sign, fg(color));
        put(buffer, at + 2, y, name, fg(super::DIM));
        at += width;
    }
}
