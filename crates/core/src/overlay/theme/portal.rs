//! The accent colour the desktop portal reports (GNOME 47+, KDE 6), for desktops whose files
//! don't say. One quick question, never waited on for long.

use std::time::Duration;

use futures::future::{Either, select};
use gpui::{BackgroundExecutor, Hsla};

use super::css::from_bytes;
use crate::units::saturating_u8;

/// How long to wait for the portal before going without: a missing or hung portal must not
/// hold the overlay back (see the `nvidia-smi` hang).
pub const PATIENCE: Duration = Duration::from_millis(300);

/// The portal's accent, or `None` if it has none or doesn't answer within [`PATIENCE`].
pub async fn accent_within(executor: &BackgroundExecutor, patience: Duration) -> Option<Hsla> {
    let asking = executor.spawn(ask());
    let giving_up = executor.timer(patience);
    match select(asking, giving_up).await {
        Either::Left((answer, _)) => answer,
        Either::Right(_) => None,
    }
}

async fn ask() -> Option<Hsla> {
    let settings = ashpd::desktop::settings::Settings::new().await.ok()?;
    let accent = settings.accent_color().await.ok()?;
    Some(from_unit_channels(accent.red(), accent.green(), accent.blue()))
}

/// A colour from red, green and blue in 0..=1, as the portal gives them.
#[must_use]
pub fn from_unit_channels(red: f64, green: f64, blue: f64) -> Hsla {
    let byte = |channel: f64| saturating_u8((channel * 255.).round());
    from_bytes(byte(red), byte(green), byte(blue), 255)
}

#[cfg(test)]
mod tests {
    use gpui::white;

    use super::*;

    #[test]
    fn full_channels_are_white() {
        assert_eq!(from_unit_channels(1., 1., 1.), white());
    }

    #[test]
    fn channels_out_of_range_are_held_to_it() {
        assert_eq!(from_unit_channels(7., -3., 1.), from_bytes(255, 0, 255, 255));
    }

    #[test]
    fn a_middle_channel_rounds_to_the_nearest_byte() {
        assert_eq!(from_unit_channels(0.5, 0., 0.), from_bytes(128, 0, 0, 255));
    }
}
