//! Runs the reclaimers for one reclaim: each one first on a dry run, whose
//! plan the log shows in one line, then for real, until the bytes freed
//! cover the need.

use std::time::SystemTime;

use super::{Bytes, Config, Reclaimed, Reclaimer, Report, Resource};
use crate::daemon::{Log, note};
use crate::job::reclaimer::{self, Ended};

/// What one run of a reclaimer gave: its report, when it printed one, and
/// why it failed, when it did.
struct Run {
    /// The report it printed.
    report: Option<Report>,
    /// Why the run failed.
    failed: Option<String>,
    /// Whether the log already said why it failed.
    logged: bool,
}

/// Asks each reclaimer of `config` in turn to free `resource`: a dry run
/// first, then, when the dry run plans to free any bytes, the real run. It
/// stops once the bytes freed cover the need. Each reclaimer's outcome
/// goes to `record`, and each failure to `log` in one line; a failure
/// moves on to the next reclaimer.
pub fn reclaim(config: &Config, resource: Resource, log: &Log, record: &mut dyn FnMut(Reclaimed)) {
    let need = resource.need(config);
    let mut freed = 0_u64;
    for reclaimer in &config.reclaimers {
        let dry = run_once(config, reclaimer, &need, true, log, record);
        let Some(plan) = dry.report.filter(|_| dry.failed.is_none()) else {
            let failed = dry.failed.unwrap_or_default();
            if !dry.logged {
                note(
                    log,
                    format_args!(
                        "the reclaimer {} failed on a dry run for {need}: {failed}",
                        reclaimer.name
                    ),
                );
            }
            record(outcome(reclaimer, &need, None, Some(failed)));
            continue;
        };
        note(
            log,
            format_args!(
                "the reclaimer {} would free {} for {need}: {}",
                reclaimer.name,
                Bytes(plan.freed_bytes),
                if plan.items.is_empty() {
                    "nothing".to_owned()
                } else {
                    plan.items.join(", ")
                }
            ),
        );
        if plan.freed_bytes == 0 {
            record(outcome(reclaimer, &need, Some(0), None));
            continue;
        }
        let real = run_once(config, reclaimer, &need, false, log, record);
        let reported = real.report.map(|report| report.freed_bytes);
        match &real.failed {
            Some(_) if real.logged => {}
            Some(failed) => note(
                log,
                format_args!(
                    "the reclaimer {} failed for {need}: {failed}",
                    reclaimer.name
                ),
            ),
            None => note(
                log,
                format_args!(
                    "the reclaimer {} freed {} for {need}",
                    reclaimer.name,
                    Bytes(reported.unwrap_or(0))
                ),
            ),
        }
        record(outcome(reclaimer, &need, reported, real.failed));
        freed = freed.saturating_add(reported.unwrap_or(0));
        if freed >= resource.need_bytes(config) {
            return;
        }
    }
}

/// Runs `reclaimer` once with `--need need`, and `--dry-run` when `dry`
/// holds. When it runs past the config's timeout, the log and `record` say
/// so at once, and the run waits until the reclaimer exits.
fn run_once(
    config: &Config,
    reclaimer: &Reclaimer,
    need: &str,
    dry: bool,
    log: &Log,
    record: &mut dyn FnMut(Reclaimed),
) -> Run {
    let mut args = vec!["--need", need];
    if dry {
        args.push("--dry-run");
    }
    let timed_out = format!(
        "ran past its timeout of {}",
        humantime::format_duration(config.timeout)
    );
    let on_timeout = || {
        note(
            log,
            format_args!(
                "the reclaimer {} {timed_out}, so the daemon stops waiting for it and starts no other reclaim until it exits",
                reclaimer.name
            ),
        );
        record(outcome(reclaimer, need, None, Some(timed_out.clone())));
    };
    match reclaimer::run(&reclaimer.command, &args, config.timeout, on_timeout) {
        Ok(Ended::Exited { code, stdout }) => {
            let parsed = Report::parse(&stdout);
            let failed = match (code, &parsed) {
                (Some(0), Ok(_)) => None,
                (Some(0), Err(error)) => Some(format!("its output is not a report: {error}")),
                (Some(code), _) => Some(format!("it exited with code {code}")),
                (None, _) => Some("a signal ended it".to_owned()),
            };
            Run {
                report: parsed.ok(),
                failed,
                logged: false,
            }
        }
        Ok(Ended::TimedOut) => Run {
            report: None,
            failed: Some(timed_out),
            logged: true,
        },
        Err(error) => Run {
            report: None,
            failed: Some(format!("it could not start: {error}")),
            logged: false,
        },
    }
}

/// Returns the record of `reclaimer`'s reclaim for `need` that ends now.
fn outcome(
    reclaimer: &Reclaimer,
    need: &str,
    freed_bytes: Option<u64>,
    failed: Option<String>,
) -> Reclaimed {
    Reclaimed {
        at: humantime::format_rfc3339_seconds(SystemTime::now()).to_string(),
        reclaimer: reclaimer.name.clone(),
        need: need.to_owned(),
        freed_bytes,
        failed,
    }
}
