//! What every screencapture feature stands on: the screens and their pixels, the selection, the
//! remembered settings, the daemon and its protocol, and the seams the features meet at.

pub mod capture;
pub mod clipboard;
pub mod config;
#[cfg(unix)]
pub mod daemon;
pub mod drawings;
#[cfg(unix)]
pub mod hyprland;
pub mod memory;
pub mod naming;
pub mod overlay;
pub mod selection;
pub mod trace;

pub use trace::{trace, trace_restart};
