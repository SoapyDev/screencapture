//! Screen recording. Nothing here runs, or is even looked up, until a recording starts.

pub mod command;
#[cfg(target_os = "linux")]
pub mod foreground;
#[cfg(all(target_os = "linux", feature = "wayland"))]
pub mod frame;
#[cfg(target_os = "linux")]
pub mod join;
#[cfg(target_os = "linux")]
pub mod load;
pub mod plan;
#[cfg(target_os = "linux")]
pub mod recorder;
#[cfg(target_os = "linux")]
pub mod session;
pub mod settings;
pub mod split;

#[cfg(target_os = "linux")]
pub use control::{Env, LiveEnv, OnScreen, Outcome, Recorder};

#[cfg(target_os = "linux")]
mod control;

/// A request to the daemon, recording ones included.
#[cfg(unix)]
pub type Request = crate::daemon::Request<command::Command>;

/// A request the daemon received, recording ones included.
#[cfg(unix)]
pub type Pending = crate::daemon::Pending<command::Command>;
