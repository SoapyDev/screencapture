//! Screen recording. Nothing here runs, or is even looked up, until a recording starts.

pub mod command;
#[cfg(target_os = "linux")]
pub mod foreground;
#[cfg(target_os = "linux")]
pub mod load;
pub mod plan;
#[cfg(target_os = "linux")]
pub mod recorder;
pub mod settings;

#[cfg(target_os = "linux")]
pub use control::{Env, LiveEnv, Outcome, Recorder};

#[cfg(target_os = "linux")]
mod control;
