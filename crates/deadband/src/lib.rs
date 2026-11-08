//! deadband keeps a busy machine usable while many heavy jobs compete for it.

pub mod admit;
pub mod cli;
pub mod client;
pub mod daemon;
pub mod environment;
pub mod job;
pub mod priority;
pub mod probe;
pub mod protocol;
pub mod socket;
#[cfg(test)]
mod testing;
pub mod throttle;

/// The name that deadband uses for itself.
pub const NAME: &str = "deadband";
