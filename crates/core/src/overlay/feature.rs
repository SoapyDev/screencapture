//! How a feature (editing, capture, recording) plugs into the overlay.
//!
//! The overlay owns the frozen screens, the selection, the windows and the bars' frames. Each
//! feature adds its keys, pointer handling, buttons and layers, and answers with an [`Effect`]
//! when the overlay itself must act: save, hand over to a recording, pick a folder.

use std::{any::Any, path::PathBuf};

use gpui::{AnyElement, App, ClickEvent, CursorStyle, Entity, Keystroke, SharedString, Window};

use super::Overlay;
use crate::{
    capture::{MonitorInfo, Rect, Snapshot},
    drawings::{Drawings, Shared},
    selection::{Request, Selection},
};

/// A point on the desktop, in logical pixels.
pub type Point = (f32, f32);

/// What the overlay must do after a feature acted.
pub enum Effect {
    /// Only the feature changed: show it.
    Repaint,
    /// Close the overlay and save what `job` makes of `request`, off the UI thread.
    Save(Request, SaveJob),
    /// Slide the bars away, close the overlay, then let `start` take over (a recording).
    HandOver(Start),
    /// Step aside for a folder picker asking `prompt`; the feature gets the folder picked.
    Browse { prompt: String, picked: Picked },
    /// Why something can't be done, shown above the action bar.
    Notice(SharedString),
}

/// Takes over once the overlay is gone.
pub type Start = Box<dyn FnOnce(HandOver, &mut App)>;

/// What a feature does with the folder picked (it gets itself as `Any`).
pub type Picked = Box<dyn FnOnce(&mut dyn Any, PathBuf)>;

/// Makes the saved file from the frozen screens, the request and any drawings.
pub type SaveJob =
    Box<dyn FnOnce(&Snapshot<&[u8]>, &Request, Option<&dyn Drawings>) -> anyhow::Result<PathBuf> + Send>;

/// What a feature taking over from the overlay gets.
pub struct HandOver {
    /// The daemon client that opened the overlay, waiting for an answer.
    #[cfg(unix)]
    pub client: Option<crate::daemon::Client>,
    /// The overlay runs in the daemon (else the process ends with the work).
    pub resident: bool,
    pub drawings: Option<Shared>,
}

/// A feature's part of the action bar: its buttons (stacked over the other features'), and
/// the drawer that opens beside them with its settings.
#[derive(Default)]
pub struct Actions {
    pub main: Vec<AnyElement>,
    pub drawer: Vec<AnyElement>,
}

/// What features may read of the overlay.
pub struct Ctx<'a> {
    pub selection: &'a Selection,
}

/// One feature's part of the overlay. Every hook has a default: a feature implements only
/// what it adds.
pub trait Feature: Any {
    /// A key while selecting: `Some` if this feature used it (the first one to use it wins).
    fn key(&mut self, _key: &Keystroke, _ctx: &Ctx) -> Option<Effect> {
        None
    }

    /// Escape: close something of its own first (`true`), before the overlay gives up.
    fn escape(&mut self) -> bool {
        false
    }

    /// A left press at `point`: `true` if this feature takes it (the selection doesn't).
    fn press(&mut self, _point: Point, _clicks: usize) -> bool {
        false
    }

    /// The pointer moved to `point`: whether anything of this feature's looks different.
    fn motion(&mut self, _point: Point) -> bool {
        false
    }

    /// The left button went up at `point`: `true` if this feature had it.
    fn release(&mut self, _point: Point) -> bool {
        false
    }

    /// Dragging something of its own: the bars hide meanwhile.
    fn busy(&self) -> bool {
        false
    }

    /// A tool in hand: the selection's handles hide, the pointer is a crosshair.
    fn holds_a_tool(&self) -> bool {
        false
    }

    /// The mouse wheel, in lines: `true` if used.
    fn scroll(&mut self, _lines: f32) -> bool {
        false
    }

    /// The kind of selection changed (region, screens, all).
    fn mode_changed(&mut self) {}

    /// The pointer's look over `point`, if this feature decides it.
    fn cursor(&self, _point: Point) -> Option<CursorStyle> {
        None
    }

    /// What it drew over the screens, for the others to keep.
    fn drawings(&self) -> Option<Shared> {
        None
    }

    /// Shown on `monitor` under the dimming, like the screen content.
    fn under_dim(&self, _monitor: &MonitorInfo, _window: &mut Window) -> Vec<AnyElement> {
        Vec::new()
    }

    /// Shown on `monitor` over the dimming.
    fn over_dim(&self, _monitor: Rect) -> Vec<AnyElement> {
        Vec::new()
    }

    /// Rows of its own under the top bar's first row.
    fn rows(&self, _act: &Act) -> Vec<AnyElement> {
        Vec::new()
    }

    /// Something opened beside the rows (a colour picker).
    fn popup(&self, _act: &Act) -> Option<AnyElement> {
        None
    }

    /// Its buttons in the bottom action bar.
    fn actions(&self, _act: &Act, _ctx: &Ctx) -> Actions {
        Actions::default()
    }

    /// Debug builds: the end-to-end tests' hook, once `region` is selected by itself.
    #[cfg(debug_assertions)]
    fn test_hook(&mut self, _region: Rect) -> Option<Effect> {
        None
    }
}

/// Makes click handlers that act on one feature of the overlay.
#[derive(Clone)]
pub struct Act {
    pub(super) overlay: Entity<Overlay>,
    pub(super) index: usize,
}

impl Act {
    /// A click handler running `act` on feature `F`, then applying its effect. Buttons stay
    /// drawn while the bars slide away, but they no longer act.
    pub fn on<F: Feature>(
        &self,
        act: impl Fn(&mut F, &Ctx) -> Effect + 'static,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        let (overlay, index) = (self.overlay.clone(), self.index);
        move |_, _, cx| {
            overlay.update(cx, |overlay, cx| overlay.act_on(index, &act, cx));
        }
    }
}
