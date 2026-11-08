//! How long the bars take to move: with reduced motion asked for, they jump instead of slide.

use std::{sync::OnceLock, time::Duration};

use serde::{Deserialize, Serialize};

use crate::config::{Config, Section};

/// The slide of a bar when motion is welcome.
const SLIDE: Duration = Duration::from_millis(150);
/// What a slide shrinks to with reduced motion: one frame, but never zero (animations divide
/// by their length).
const JUMP: Duration = Duration::from_millis(1);
/// The environment variable that asks for reduced motion (any value but empty or `0`).
pub const ENVIRONMENT: &str = "SCREENCAPTURE_REDUCED_MOTION";

/// The `reduced_motion` key of the config file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Motion {
    #[serde(default)]
    pub reduced_motion: bool,
}

impl Section for Motion {}

/// Whether reduced motion was asked for: by the environment, else by the config.
#[must_use]
pub fn reduced(environment: Option<&str>, configured: bool) -> bool {
    environment.map_or(configured, |value| !value.is_empty() && value != "0")
}

/// The length of a slide, for `reduced` motion or not.
#[must_use]
pub const fn slide_length(reduced: bool) -> Duration {
    if reduced { JUMP } else { SLIDE }
}

/// The length of a slide for this run: asked for once, when the first bar moves.
#[must_use]
pub fn slide() -> Duration {
    static LENGTH: OnceLock<Duration> = OnceLock::new();
    *LENGTH.get_or_init(|| {
        let environment = std::env::var(ENVIRONMENT).ok();
        let configured = Config::load().section::<Motion>().reduced_motion;
        slide_length(reduced(environment.as_deref(), configured))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slide_takes_its_time_by_default() {
        assert_eq!(slide_length(false), Duration::from_millis(150));
    }

    #[test]
    fn reduced_motion_makes_a_slide_a_jump() {
        assert!(slide_length(true) <= Duration::from_millis(1));
    }

    #[test]
    fn a_jump_is_never_zero_long() {
        assert!(!slide_length(true).is_zero());
    }

    #[test]
    fn the_config_asks_for_reduced_motion() {
        assert!(reduced(None, true));
    }

    #[test]
    fn the_environment_beats_the_config() {
        assert!(!reduced(Some("0"), true));
    }

    #[test]
    fn the_environment_asks_for_reduced_motion() {
        assert!(reduced(Some("1"), false));
    }

    #[test]
    fn an_empty_environment_value_asks_for_nothing() {
        assert!(!reduced(Some(""), false));
    }
}
