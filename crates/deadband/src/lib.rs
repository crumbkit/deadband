//! deadband keeps a busy machine usable while many heavy jobs compete for it.

pub mod cli;
pub mod probe;

/// The name that deadband uses for itself.
pub const NAME: &str = "deadband";
