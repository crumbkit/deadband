//! `deadband top`: a read-only terminal timeline of the last ten minutes.

//!
//! The view polls the daemon and the local probe once a second, and keeps
//! nothing beyond the last reply and 600 samples of the load and the hold.

use std::fmt;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use clap::CommandFactory as _;
use crumbkit::{Call, Code, Failure, Recovery, RenderCall as _};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyModifiers};

use crate::cli::{Cli, Machine};
use crate::client;
use crate::probe::Probe;

mod guard;
mod model;
#[cfg(test)]
mod tests;
mod view;

use self::guard::{Guard, Modes};
use self::model::Model;

/// The code of `deadband top` that has no terminal to draw on.
pub const TOP_NOT_A_TERMINAL: Code = Code::new("deadband", "top", "not_a_terminal");

/// The code of a terminal that `deadband top` could not use.
pub const TOP_TERMINAL_FAILED: Code = Code::new("deadband", "top", "terminal_failed");

/// How long one poll waits for the daemon. Keys wait at most this long.
const POLL_WITHIN: Duration = Duration::from_millis(500);

/// How often the view polls the daemon and samples the machine.
const POLL_EVERY: Duration = Duration::from_secs(1);

/// The exit code of a view that Ctrl-C ended (CLI-8).
pub const EXIT_INTERRUPTED: u8 = 130;

/// The command that starts the daemon, rendered from the command tree once.
static DAEMON_CALL: LazyLock<String> = LazyLock::new(|| {
    // A test asserts that the call renders, so the words after `or` show
    // only if `daemon` leaves the tree without that test noticing.
    Call::new(&["daemon"])
        .render(&mut Cli::command())
        .unwrap_or_else(|_| "deadband daemon".to_owned())
});

/// Why `deadband top` did not run or stopped.
#[derive(Debug, Failure)]
pub enum TopFailed {
    /// Standard input or output is not a terminal, or `--json` was given.
    #[failure(code = TOP_NOT_A_TERMINAL, recovery = Recovery::Run(Call::new(&["status"])))]
    NotATerminal,
    /// Reading or writing the terminal failed.
    #[failure(code = TOP_TERMINAL_FAILED, recovery = Recovery::Retry)]
    Terminal(io::Error),
}

impl fmt::Display for TopFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotATerminal => f.write_str(
                "deadband top draws on a terminal, and standard input or output is not one, or --json was given",
            ),
            Self::Terminal(error) => write!(f, "deadband top could not use the terminal: {error}"),
        }
    }
}

impl std::error::Error for TopFailed {}

/// Returns the command that starts the daemon, such as `deadband daemon`,
/// which the no-daemon screen names.
fn daemon_call() -> &'static str {
    &DAEMON_CALL
}

/// Refuses to open unless both streams are terminals and no `--json` was
/// given.
fn opens(stdin: bool, stdout: bool, json: bool) -> Result<(), TopFailed> {
    if stdin && stdout && !json {
        Ok(())
    } else {
        Err(TopFailed::NotATerminal)
    }
}

/// Returns the exit code when `event` quits the view: 0 for `q`, and 130 for
/// Ctrl-C, which arrives as a key in raw mode (CLI-8).
fn quits(event: &Event) -> Option<u8> {
    let Event::Key(key) = event else {
        return None;
    };
    if key.code == KeyCode::Char('q') {
        Some(0)
    } else if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        Some(EXIT_INTERRUPTED)
    } else {
        None
    }
}

/// Polls the daemon and samples the machine once, into `model`.
fn tick(model: &mut Model, socket: &Path, probe: &impl Probe, within: Duration) {
    let polled = client::timeline(socket, within);
    let load = probe.sample().ok().map(|sample| sample.load_1m);
    model.observe(Instant::now(), polled, load);
}

/// The real terminal: raw mode and the alternate screen, through ratatui.
struct Terminal;

impl Modes for Terminal {
    type Screen = DefaultTerminal;

    /// Enters both modes. ratatui also installs its panic hook, which
    /// restores the terminal before the panic's message prints.
    fn enter(&mut self) -> io::Result<DefaultTerminal> {
        ratatui::try_init()
    }

    fn leave(&mut self) {
        ratatui::restore();
    }
}

/// Polls once a second and draws, and waits for keys in between, until the
/// person quits. A poll waits at most [`POLL_WITHIN`], so a key waits at
/// most that long.
fn watch<P: Probe>(
    screen: &mut DefaultTerminal,
    model: &mut Model,
    machine: &Machine<P>,
) -> io::Result<u8> {
    loop {
        let next = Instant::now() + POLL_EVERY;
        tick(model, &machine.socket, &machine.probe, POLL_WITHIN);
        screen.draw(|frame| view::draw(frame.buffer_mut(), model, Instant::now()))?;
        loop {
            let wait = next.saturating_duration_since(Instant::now());
            if wait.is_zero() || !event::poll(wait)? {
                break;
            }
            let event = event::read()?;
            if let Some(code) = quits(&event) {
                return Ok(code);
            }
            if matches!(event, Event::Resize(..)) {
                screen.draw(|frame| view::draw(frame.buffer_mut(), model, Instant::now()))?;
            }
        }
    }
}

/// Runs `deadband top` against `machine` until the person quits, and
/// returns the exit code. `json` says that `--json` was given, which the
/// view refuses.
///
/// # Errors
///
/// Returns why the view could not open or why it stopped.
pub fn run<P: Probe>(machine: &Machine<P>, json: bool) -> Result<u8, TopFailed> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the view owns the terminal, and asks only whether its streams are terminals"
    )]
    let streams = (
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    );
    opens(streams.0, streams.1, json)?;
    let (_guard, mut screen) = Guard::enter(Terminal).map_err(TopFailed::Terminal)?;
    let mut model = Model::new(machine.cores);
    watch(&mut screen, &mut model, machine).map_err(TopFailed::Terminal)
}
