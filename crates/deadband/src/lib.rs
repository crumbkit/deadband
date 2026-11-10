//! deadband keeps a busy machine usable while many heavy jobs compete for it.

pub mod admission;
pub mod admit;
pub mod cli;
pub mod client;
pub mod daemon;
pub mod environment;
pub mod history;
pub mod job;
pub mod name;
pub mod priority;
pub mod probe;
pub mod protocol;
pub mod reclaim;
pub mod serve;
pub mod socket;
pub mod stuck;
#[cfg(test)]
mod testing;
pub mod throttle;
#[cfg(feature = "tui")]
pub mod tui;

/// The name that deadband uses for itself.
pub const NAME: &str = "deadband";
