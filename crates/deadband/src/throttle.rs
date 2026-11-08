//! The only module that changes the priority of a job. deadband never
//! signals or ends a job.
//!
//! `deadband run` lowers its own priority before it starts a normal-lane
//! job, and the job inherits it.

use std::io;

use rustix::process::{getpriority_process, setpriority_process};

#[cfg(test)]
mod tests;

/// The nice value of a job in the normal lane.
pub const NORMAL_NICE: i32 = 5;

/// Returns this process's nice value.
///
/// # Errors
///
/// Returns the error of a reading that failed.
pub fn own_priority() -> io::Result<i32> {
    Ok(getpriority_process(None)?)
}

/// Sets this process's nice value to `nice`, unless it is already higher,
/// so the call never raises the process's priority.
///
/// # Errors
///
/// Returns the error of a reading or setting that failed.
pub fn lower_own_priority(nice: i32) -> io::Result<()> {
    let current = own_priority()?;
    if current < nice {
        #[expect(
            clippy::disallowed_methods,
            reason = "crates/deadband/src/throttle.rs is the module that changes a job's priority"
        )]
        setpriority_process(None, nice)?;
    }
    Ok(())
}
