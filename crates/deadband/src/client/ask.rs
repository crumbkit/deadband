//! Asking the daemon to start a job, and waiting while the daemon holds it
//! in its queue.
//!
//! The `Ask` line and the first answer share one deadline. While the job
//! waits, a daemon that promised beats in its `Queued` reply must send a
//! line within [`BEAT_SILENCE`], or the client counts it as hung and starts
//! the job. A daemon that promised none is waited on with no deadline, as
//! an older daemon sends no beats.

use std::io::BufReader;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use super::{
    Admission, BEAT_SILENCE, Deadline, Lease, Notices, PeerUid, REASK_EVERY, REASK_FOR, Unasked,
    exchange, peer_uid, reply, wrong_reply,
};
use crate::admit::{Hold, Threads};
use crate::protocol::{Job, Reply, Request};

/// How long a client waits at each step of an ask.
#[derive(Debug, Clone, Copy)]
pub(super) struct Waits {
    /// How long the daemon has to take the `Ask` line and send its first
    /// answer.
    pub(super) answer: Duration,
    /// How long a queued client keeps asking after its connection closes.
    pub(super) reask: Duration,
    /// How long a queued client whose daemon promised beats waits for a
    /// line.
    pub(super) silence: Duration,
}

impl Waits {
    /// Returns the waits of a client whose first answer must arrive within
    /// `answer`, with the default re-ask window and beat silence.
    const fn answering_within(answer: Duration) -> Self {
        Self {
            answer,
            reask: REASK_FOR,
            silence: BEAT_SILENCE,
        }
    }
}

/// Asks the daemon at `path` to start `job`, and waits until it does.
///
/// The daemon must take the `Ask` line and send its first answer within
/// `answer_within`. When the job waits, `queued` receives how many jobs
/// start before it and why the first of them waits. The client then waits
/// for the grant while the daemon keeps the connection open and, when it
/// promised beats, sends a line at least once per [`BEAT_SILENCE`].
pub fn ask(
    path: &Path,
    job: &Job,
    answer_within: Duration,
    queued: impl FnOnce(usize, Option<Hold>),
) -> Admission {
    ask_noticed(path, job, answer_within, queued, None)
}

/// Asks as [`ask`] does, and passes each line about the job's activity to
/// `notices` while the job runs.
pub fn ask_noticed(
    path: &Path,
    job: &Job,
    answer_within: Duration,
    queued: impl FnOnce(usize, Option<Hold>),
    notices: Option<&Notices>,
) -> Admission {
    let waits = Waits::answering_within(answer_within);
    ask_waiting(path, job, waits, queued, notices, peer_uid)
}

/// Asks as [`ask`] does, with `waits`.
#[cfg(test)]
pub(super) fn ask_reasking(
    path: &Path,
    job: &Job,
    waits: Waits,
    queued: impl FnOnce(usize, Option<Hold>),
    notices: Option<&Notices>,
) -> Admission {
    ask_waiting(path, job, waits, queued, notices, peer_uid)
}

/// Asks as [`ask`] does, and reads the owner of the daemon with `peer`.
#[cfg(test)]
pub(super) fn ask_checked(
    path: &Path,
    job: &Job,
    answer_within: Duration,
    queued: impl FnOnce(usize, Option<Hold>),
    notices: Option<&Notices>,
    peer: PeerUid,
) -> Admission {
    let waits = Waits::answering_within(answer_within);
    ask_waiting(path, job, waits, queued, notices, peer)
}

/// Asks as [`ask`] does with `waits`, reads the owner of the daemon with
/// `peer`, and keeps asking for `waits.reask` after a queued job loses its
/// connection.
fn ask_waiting(
    path: &Path,
    job: &Job,
    waits: Waits,
    queued: impl FnOnce(usize, Option<Hold>),
    notices: Option<&Notices>,
    peer: PeerUid,
) -> Admission {
    let mut queued = Some(queued);
    // When the connection of a queued job closed. A daemon that queues the
    // job again and then sends a beat clears it, so each loss of a daemon
    // that served the job opens a window of its own. A daemon that queues
    // the job and closes before it beats leaves the earlier loss in place.
    let mut lost: Option<Instant> = None;
    loop {
        match try_ask(path, job, waits, &mut queued, &mut lost, notices, peer) {
            Ok(lease) => return Admission::Granted(lease),
            Err(unasked) if lost.is_none() => return Admission::Open(unasked),
            Err(_) => {}
        }
        // No daemon kept the job in time, so it starts, and the line says
        // the connection that queued it closed.
        if lost.is_some_and(|at| at.elapsed() >= waits.reask) {
            return Admission::Open(Unasked::Closed);
        }
        pause();
    }
}

/// Waits [`REASK_EVERY`] before the next try to reach a daemon.
fn pause() {
    #[expect(
        clippy::disallowed_methods,
        reason = "the pause between tries to reach a daemon, which has no event to wait on; REASK_EVERY names it"
    )]
    thread::sleep(REASK_EVERY);
}

/// Asks once as [`ask`] does, and returns the lease or why there is none.
/// `queued` is taken the first time the daemon queues the job. When the
/// connection closes after the daemon queued the job, `lost` gets that
/// moment unless it holds an earlier one.
fn try_ask(
    path: &Path,
    job: &Job,
    waits: Waits,
    queued: &mut Option<impl FnOnce(usize, Option<Hold>)>,
    lost: &mut Option<Instant>,
    notices: Option<&Notices>,
    peer: PeerUid,
) -> Result<Lease, Unasked> {
    let (mut reader, first) = exchange(path, &Request::Ask(job.clone()), waits.answer, peer)?;
    let threads = match first {
        Reply::Granted { threads } => threads,
        Reply::Queued { ahead, held, beats } => {
            if let Some(queued) = queued.take() {
                queued(ahead, held);
            }
            let silence = beats.then_some(waits.silence);
            match wait_for_grant(&mut reader, silence, lost) {
                Err(Unasked::Closed) => {
                    lost.get_or_insert_with(Instant::now);
                    return Err(Unasked::Closed);
                }
                waited => waited?,
            }
        }
        _ => return Err(wrong_reply()),
    };
    let stream = reader.into_inner().stream;
    Ok(Lease::watched(stream, threads, path, job, notices))
}

/// Reads the lines of a daemon that queued the job until one grants it, and
/// returns the grant's threads. With a `silence`, each line must arrive
/// within it, and a beat clears `lost`. Without one, the reads wait for as
/// long as the connection stays open.
fn wait_for_grant(
    reader: &mut BufReader<Deadline>,
    silence: Option<Duration>,
    lost: &mut Option<Instant>,
) -> Result<Threads, Unasked> {
    loop {
        reader.get_mut().at = silence.map(|silence| Instant::now() + silence);
        match reply(reader, silence.unwrap_or_default()) {
            Ok(Reply::Granted { threads }) => return Ok(threads),
            Ok(Reply::Beat) => *lost = None,
            Ok(_) => return Err(wrong_reply()),
            Err(Unasked::Silent { waited }) => return Err(Unasked::Hung { waited }),
            Err(unasked) => return Err(unasked),
        }
    }
}
