//! With `SCREENCAPTURE_TRACE=1`, timings of each milestone on stderr.

use std::{
    sync::{Mutex, OnceLock},
    time::Instant,
};

static START: Mutex<Option<Instant>> = Mutex::new(None);

/// With `SCREENCAPTURE_TRACE=1`, print milliseconds since launch (or, in the daemon, since the
/// request arrived) for each milestone.
pub fn trace(what: &str) {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    if *ENABLED.get_or_init(|| std::env::var_os("SCREENCAPTURE_TRACE").is_some()) {
        let start = *START.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let ms = start.map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0);
        eprintln!("[{ms:7.1} ms] {what}");
    }
}

pub fn trace_restart() {
    *START.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
}
