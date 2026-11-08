//! The recording feature's part of the overlay and the daemon: the Record button and settings,
//! the frame and controls while recording, and the daemon's `record …` requests.

#[cfg(feature = "wayland")]
mod controls;
#[cfg(feature = "wayland")]
mod drawer;
mod frame;
pub mod launch;
mod recordings;
pub mod service;

pub use recordings::Recordings;
