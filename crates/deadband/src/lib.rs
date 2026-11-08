//! deadband keeps a busy machine usable while many heavy jobs compete for it.

pub mod admit;
pub mod cli;
pub mod client;
pub mod daemon;
pub mod job;
pub mod probe;
pub mod protocol;
pub mod socket;
#[cfg(test)]
mod testing;

/// The name that deadband uses for itself.
pub const NAME: &str = "deadband";
