//! Picks the overlay's colours: the config, the desktop's files, and (when those have no
//! accent) the portal's, asked off the first frame's path. Found once per program run, so a
//! resident daemon does no disk or D-Bus work on later captures.

use std::sync::{Arc, PoisonError, RwLock};

use gpui::{App, Entity};

use super::{
    Overlay,
    theme::{
        self, Scheme,
        choice::ThemeChoice,
        desktop::{Desktop, DesktopColours, DiskFiles, config_dir, file_sources},
    },
};
use crate::config::Config;

/// What was found of the desktop's colours, for as long as the program runs. The daemon reads
/// it on the first capture only; a change on disk needs a restart.
static FOUND: RwLock<Option<Arc<DesktopColours>>> = RwLock::new(None);

/// Paint the overlay in the desktop's colours from now on.
pub fn follow_desktop(overlay: &Entity<Overlay>, cx: &App) {
    let colours = found_colours(overlay, cx);
    theme::set_current(colours.theme(Scheme::from(cx.window_appearance())));
}

/// The colours found at the first call; the later calls only hand them back.
fn found_colours(overlay: &Entity<Overlay>, cx: &App) -> Arc<DesktopColours> {
    if let Some(kept) = kept() {
        return kept;
    }
    let colours = Arc::new(read_desktop_colours());
    keep(&colours);
    #[cfg(target_os = "linux")]
    if colours.wants_portal_accent() {
        ask_the_portal_for_an_accent(&colours, overlay, cx);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (overlay, cx);
    colours
}

fn kept() -> Option<Arc<DesktopColours>> {
    FOUND.read().unwrap_or_else(PoisonError::into_inner).clone()
}

fn keep(colours: &Arc<DesktopColours>) {
    *FOUND.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(colours));
}

/// The config and the colour files, read now.
fn read_desktop_colours() -> DesktopColours {
    let choice = Config::load().section::<ThemeChoice>();
    let current = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let desktop = Desktop::from_current_desktop(&current);
    let sources = config_dir().map_or_else(Vec::new, |dir| {
        file_sources(choice.theme_source, desktop, &dir, &DiskFiles)
    });
    DesktopColours::new(choice, sources)
}

/// Once the portal answers (if it does, in time), keep its accent and repaint.
#[cfg(target_os = "linux")]
fn ask_the_portal_for_an_accent(colours: &Arc<DesktopColours>, overlay: &Entity<Overlay>, cx: &App) {
    let (colours, overlay) = (Arc::clone(colours), overlay.clone());
    cx.spawn(async move |cx| {
        let answer = theme::portal::accent_within(cx.background_executor(), theme::portal::PATIENCE).await;
        let Some(accent) = answer else {
            return;
        };
        keep(&Arc::new(colours.with_fallback_accent(accent)));
        cx.update(|cx| follow_desktop(&overlay, cx));
        cx.update(|cx| overlay.update(cx, |_, cx| cx.notify()));
    })
    .detach();
}
