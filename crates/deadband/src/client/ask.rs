//! Asking the daemon to start a job, and waiting while the daemon holds it
//! in its queue.

use std::io::BufReader;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use super::{
    Admission, BEAT_SILENCE, Lease, Notices, PeerUid, REASK_EVERY, REASK_FOR, Unasked,
    connect_checked, peer_uid, reply, wrong_reply,
};
use crate::admit::Hold;
use crate::protocol::{Job, Reply, Request, write_line};

/// How long a client waits at each step of an ask.
#[derive(Debug, Clone, Copy)]
pub(super) struct Waits {
    /// How long the daemon has to take the `Ask` line and send its first
    /// answer.
    pub(super) answer: Duration,
    /// How long a queued client keeps asking after its connection closes.
    pub(super) reask: Duration,
    /// How long a queued client whose daemon sends beats waits for a line.
    pub(super) silence: Duration,
}

impl Waits {
    /// Returns the waits of a client whose first answer must arrive within
    /// `answer`, with the default re-ask window and beat silence.
    pub(super) const fn answering_within(answer: Duration) -> Self {
        Self {
            answer,
            reask: REASK_FOR,
            silence: BEAT_SILENCE,
        }
    }
}

/// Asks the daemon at `path` to start `job`, and waits until it does.
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

/// Asks as [`ask`] does, with the waits of `waits`.
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

fn ask_waiting(
    path: &Path,
    job: &Job,
    waits: Waits,
    queued: impl FnOnce(usize, Option<Hold>),
    notices: Option<&Notices>,
    peer: PeerUid,
) -> Admission {
    let mut queued = Some(queued);
    let mut lost: Option<Instant> = None;
    loop {
        let mut was_queued = false;
        match try_ask(path, job, waits.answer, &mut queued, &mut was_queued, notices, peer) {
            Ok(lease) => return Admission::Granted(lease),
            Err(Unasked::Closed) if was_queued => {
                lost.get_or_insert_with(Instant::now);
            }
            Err(unasked) if lost.is_none() => return Admission::Open(unasked),
            Err(_) => {}
        }
        if lost.is_some_and(|at| at.elapsed() >= waits.reask) {
            return Admission::Open(Unasked::Closed);
        }
        pause();
    }
}

fn pause() {
    #[expect(
        clippy::disallowed_methods,
        reason = "the pause between tries to reach a daemon, which has no event to wait on; REASK_EVERY names it"
    )]
    thread::sleep(REASK_EVERY);
}

fn try_ask(
    path: &Path,
    job: &Job,
    answer_within: Duration,
    queued: &mut Option<impl FnOnce(usize, Option<Hold>)>,
    was_queued: &mut bool,
    notices: Option<&Notices>,
    peer: PeerUid,
) -> Result<Lease, Unasked> {
    let mut stream = connect_checked(path, peer)?;
    let garbled = |error| Unasked::Garbled { error };
    stream
        .set_read_timeout(Some(answer_within))
        .map_err(garbled)?;
    write_line(&mut stream, &Request::Ask(job.clone())).map_err(|_| Unasked::Closed)?;
    let mut reader = BufReader::new(stream.try_clone().map_err(garbled)?);
    match reply(&mut reader, answer_within)? {
        Reply::Granted { threads: n } => return Ok(Lease::watched(stream, n, path, job, notices)),
        Reply::Queued { ahead, held, .. } => {
            *was_queued = true;
            if let Some(queued) = queued.take() {
                queued(ahead, held);
            }
        }
        _ => return Err(wrong_reply()),
    }
    let _ = stream.set_read_timeout(None);
    match reply(&mut reader, answer_within)? {
        Reply::Granted { threads } => Ok(Lease::watched(stream, threads, path, job, notices)),
        _ => Err(wrong_reply()),
    }
}
