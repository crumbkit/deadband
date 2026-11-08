//! Reads the machine and the disk of the home directory for the daemon,
//! and logs when a reading starts failing or reads again.

use std::path::Path;

use super::{Log, Settings, note};
use crate::admit::Reading;
use crate::probe::{Disk, Probe};

/// Reads the machine with `probe`, and the disk of the settings' home. It
/// writes a log line when the probe or the disk starts failing or reads
/// again, judged against `before`, the previous reading.
pub(super) fn read_machine(
    probe: &impl Probe,
    settings: &Settings,
    log: &Log,
    before: Option<Option<Reading>>,
) -> Option<Reading> {
    match probe.sample() {
        Ok(sample) => {
            if before == Some(None) {
                note(log, format_args!("the probe reads the machine again"));
            }
            let disk = settings
                .home
                .as_ref()
                .and_then(|home| read_disk(probe, home, log, before));
            Some(Reading {
                pressure: sample.memory_pressure,
                load_1m: sample.load_1m,
                disk,
            })
        }
        Err(error) => {
            if before.is_none_or(|before| before.is_some()) {
                note(
                    log,
                    format_args!(
                        "{error}, so each job starts once a place is free until it reads again"
                    ),
                );
            }
            None
        }
    }
}

/// Reads the disk that holds `home` with `probe`. It writes a log line
/// when the disk starts failing or reads again, judged against `before`.
fn read_disk(
    probe: &impl Probe,
    home: &Path,
    log: &Log,
    before: Option<Option<Reading>>,
) -> Option<Disk> {
    let failing_before = matches!(before, Some(Some(Reading { disk: None, .. })));
    match probe.disk(home) {
        Ok(disk) => {
            if failing_before {
                note(
                    log,
                    format_args!("the disk of {} reads again", home.display()),
                );
            }
            Some(disk)
        }
        Err(error) => {
            if !failing_before {
                note(
                    log,
                    format_args!(
                        "deadband could not read the disk of {}: {error}, so the disk holds no job until it reads again",
                        home.display()
                    ),
                );
            }
            None
        }
    }
}

/// Reports whether the priority file at `file` holds at least one entry.
/// A missing or unreadable file holds none.
pub(super) fn priority_active(file: Option<&Path>) -> bool {
    file.and_then(|file| std::fs::read_to_string(file).ok())
        .is_some_and(|words| crate::priority::active(&words))
}
