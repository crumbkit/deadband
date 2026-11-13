//! deadband's tool record and exit codes.

use crumbkit::Tool;
use crumbkit::commands::COMMANDS_LIST;
use crumbkit::completion::{COMPLETION_CHECK, COMPLETION_INSTALL, COMPLETION_PRINT};
use crumbkit::help::{Effects, Example, FileFormat, Goal, Group, Help, Leaf};
use crumbkit::schemas::{SCHEMAS_LIST, SCHEMAS_SHOW};
use crumbkit::tool::{Exit, Failures};
use crumbkit_skills::{SKILLS_CHECK, SKILLS_INSTALL, SKILLS_PRINT};

use super::commands::{JOB_NOT_FOUND, JOB_NOT_STARTED};
use super::service::{
    SERVICE_INSTALL, SERVICE_LAUNCHCTL_FAILED, SERVICE_LOCKED, SERVICE_PLIST_UNREADABLE,
    SERVICE_UNINSTALL, SERVICE_UNLOCATED, SERVICE_UNSUPPORTED, SERVICE_WRITE_FAILED,
};
use super::status::STATUS;
use crate::daemon::SOCKET_UNUSABLE;
use crate::probe::{MACHINE_UNREADABLE, MACHINE_UNSUPPORTED};
use crate::reclaim::CONFIG;
#[cfg(feature = "tui")]
use crate::tui::{EXIT_INTERRUPTED, TOP_NOT_A_TERMINAL, TOP_TERMINAL_FAILED};

/// The exit code of a run that could not read the machine.
pub const EXIT_MACHINE: u8 = 3;

/// The exit code of a daemon that could not listen on its socket.
pub const EXIT_DAEMON: u8 = 4;

/// The exit code of a `deadband top` that has no terminal or lost it.
#[cfg(feature = "tui")]
pub const EXIT_TOP: u8 = 5;

/// The exit code of a `deadband service` command that could not install or
/// uninstall the launchd agent.
pub const EXIT_SERVICE: u8 = 6;

/// The exit code of a `deadband run` whose program was found but could not
/// start, as `env` and `timeout` report it.
pub const EXIT_NOT_STARTED: u8 = 126;

/// The exit code of a `deadband run` whose program was not found, as `env`
/// and `timeout` report it.
pub const EXIT_NOT_FOUND: u8 = 127;

/// deadband's tool record: the examples, effects and document of each
/// command, and the exit codes with the failures that produce them.
pub const TOOL: Tool = Tool::new()
    .help(
        Help::new()
            .groups(&[Group::new(
                &[],
                &[
                    Goal::new("Run a heavy job when the machine has room", &["run"]),
                    Goal::new("See the machine and its jobs", &["status", "top"]),
                    Goal::new("Admit jobs", &["daemon"]),
                    Goal::new("Start the daemon at login", &["service"]),
                    Goal::new("Explore the commands", &["commands", "schemas"]),
                    Goal::new("Install deadband's skill", &["skills"]),
                    Goal::new("Set up shell completion", &["completion"]),
                ],
            )
            .compact()])
            .leaves(&[
                Leaf::new(&["status"])
                    .examples(&[
                        Example::new(&[], "Print the machine's load, memory pressure and swap"),
                        Example::new(&["--json"], "Print the same sample as a JSON document"),
                    ])
                    .effects(Effects::read_only())
                    .document(STATUS),
                Leaf::new(&["run"])
                    .examples(&[Example::new(
                        &["--label", "tests", "--", "cargo", "test"],
                        "Run cargo test once the daemon admits it",
                    )])
                    .effects(Effects::additive().all_or_none().open_world())
                    .passes_on_exit("the job")
                    .prints_no_result(),
                Leaf::new(&["daemon"])
                    .examples(&[Example::new(
                        &["--max-jobs", "2"],
                        "Admit jobs while at most 2 run at once",
                    )])
                    .effects(Effects::additive())
                    .prints_no_result(),
                Leaf::new(&["service", "install"])
                    .examples(&[
                        Example::new(
                            &[],
                            "Install the launchd agent that runs the daemon at login",
                        ),
                        Example::new(&["--dry-run"], "Print the plist and the launchctl steps"),
                    ])
                    .effects(Effects::additive().idempotent())
                    .document(SERVICE_INSTALL),
                Leaf::new(&["service", "uninstall"])
                    .examples(&[Example::new(
                        &["--yes"],
                        "Unload the launchd agent and delete its plist without asking",
                    )])
                    .effects(Effects::destructive().idempotent())
                    .document(SERVICE_UNINSTALL),
                #[cfg(feature = "tui")]
                Leaf::new(&["top"])
                    .examples(&[Example::new(
                        &[],
                        "Show the last ten minutes of jobs in the terminal",
                    )])
                    .effects(Effects::read_only())
                    .prints_no_result(),
                Leaf::new(&["commands", "list"])
                    .examples(&[
                        Example::new(&[], "List every command with its arguments"),
                        Example::new(&["run"], "List only `deadband run` and its arguments"),
                    ])
                    .document(COMMANDS_LIST)
                    .effects(Effects::read_only()),
                Leaf::new(&["schemas", "list"])
                    .examples(&[Example::new(
                        &[],
                        "List the documents deadband prints and its config file format",
                    )])
                    .document(SCHEMAS_LIST)
                    .effects(Effects::read_only()),
                Leaf::new(&["schemas", "show"])
                    .examples(&[Example::new(
                        &["deadband/config/v1"],
                        "Print the JSON Schema of the daemon's config file",
                    )])
                    .document(SCHEMAS_SHOW)
                    .effects(Effects::read_only()),
                Leaf::new(&["skills", "install"])
                    .examples(&[
                        Example::new(
                            &[],
                            "Install deadband's skill into each agent skills directory that exists",
                        ),
                        Example::new(
                            &["--force"],
                            "Install it, replacing a file or a link that install did not write",
                        ),
                    ])
                    .effects(Effects::additive().idempotent())
                    .document(SKILLS_INSTALL),
                Leaf::new(&["skills", "check"])
                    .examples(&[Example::new(
                        &[],
                        "Report each agent skills directory whose copy is missing or differs",
                    )])
                    .effects(Effects::read_only())
                    .document(SKILLS_CHECK),
                Leaf::new(&["skills", "print"])
                    .examples(&[Example::new(&[], "Print deadband's skill")])
                    .effects(Effects::read_only())
                    .document(SKILLS_PRINT),
                Leaf::new(&["completion", "install"])
                    .examples(&[
                        Example::new(&["--shell", "fish"], "Install deadband's completion for fish"),
                        Example::new(
                            &["--shell", "zsh", "--force"],
                            "Install it for zsh, replacing a file or a link that install did not write",
                        ),
                    ])
                    .effects(Effects::additive().idempotent())
                    .document(COMPLETION_INSTALL),
                Leaf::new(&["completion", "check"])
                    .examples(&[Example::new(
                        &["--shell", "zsh"],
                        "Report zsh's completion file when it is missing or differs",
                    )])
                    .effects(Effects::read_only())
                    .document(COMPLETION_CHECK),
                Leaf::new(&["completion", "print"])
                    .examples(&[Example::new(
                        &["--shell", "fish"],
                        "Print the registration script for fish",
                    )])
                    .effects(Effects::read_only())
                    .document(COMPLETION_PRINT),
            ])
            .file_formats(&[FileFormat::new(CONFIG)]),
    )
    .failures(
        Failures::new().exits(&[
            Exit::new(EXIT_MACHINE, "deadband could not read the machine.")
                .failures(&[&MACHINE_UNREADABLE, &MACHINE_UNSUPPORTED]),
            Exit::new(EXIT_DAEMON, "the daemon could not listen on its socket.")
                .failures(&[&SOCKET_UNUSABLE]),
            Exit::new(
                EXIT_SERVICE,
                "deadband could not install or uninstall its launchd agent.",
            )
            .commands(&[&["service", "install"], &["service", "uninstall"]])
            .failures(&[
                &SERVICE_UNSUPPORTED,
                &SERVICE_UNLOCATED,
                &SERVICE_PLIST_UNREADABLE,
                &SERVICE_WRITE_FAILED,
                &SERVICE_LAUNCHCTL_FAILED,
                &SERVICE_LOCKED,
            ]),
            Exit::new(
                EXIT_NOT_STARTED,
                "the program given to `run` was found but could not start.",
            )
            .commands(&[&["run"]])
            .failures(&[&JOB_NOT_STARTED]),
            Exit::new(EXIT_NOT_FOUND, "the program given to `run` was not found.")
                .commands(&[&["run"]])
                .failures(&[&JOB_NOT_FOUND]),
            #[cfg(feature = "tui")]
            Exit::new(EXIT_TOP, "`top` has no terminal to draw on, or lost it.")
                .commands(&[&["top"]])
                .failures(&[&TOP_NOT_A_TERMINAL, &TOP_TERMINAL_FAILED]),
            #[cfg(feature = "tui")]
            Exit::new(EXIT_INTERRUPTED, "the person pressed Ctrl-C in `top`.")
                .commands(&[&["top"]]),
        ]),
    );
