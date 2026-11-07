//! The bottom action bar: what to do with the selection (record it, capture it) and how to
//! record it.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, Context, Div, Entity, SharedString, Stateful, div, ease_in_out,
    prelude::*, px, rgba,
};

use super::{Overlay, Phase, folder::Folder, layout::Edge, tools};
use crate::{
    record::{
        command::Command,
        plan::{self, Target},
        settings::{Choice, Overrides, Settings},
    },
    selection::{Mode, Request},
};

/// How long the bars take to slide off the screens when a recording starts.
pub const SLIDE: Duration = Duration::from_millis(150);
/// Distance of the bars from their screen edge.
pub const EDGE_GAP: f32 = 24.;
/// Room the action bar takes at the bottom: a toolbar at the bottom sits above it.
pub const ACTION_BAR_ROOM: f32 = 48.;
/// How far the bars travel when they slide away: past any screen edge.
const SLIDE_DISTANCE: f32 = 240.;

/// What the action bar shows, read from the overlay before building it.
pub struct ActionBar<'a> {
    pub mode: Mode,
    /// Picked screens, or 1 for a region; 0 when there is nothing to capture yet.
    pub ready: usize,
    pub settings: Settings,
    /// Where videos are saved, as the folder button shows it.
    pub videos: SharedString,
    pub notice: Option<&'a SharedString>,
    pub leaving: bool,
}

/// The bar: record, capture, and the recording settings.
pub fn action_bar(overlay: &Entity<Overlay>, bar: &ActionBar<'_>) -> AnyElement {
    let row = super::panel()
        .child(
            tools::button("record", "● Record  V", false)
                .on_click(tools::on(overlay, |o, cx| o.record(false, cx))),
        )
        .child(
            tools::button("capture", capture_label(bar), bar.ready > 0)
                .on_click(tools::on(overlay, Overlay::confirm)),
        )
        .child(super::separator())
        .children(setting_buttons(overlay, bar.settings))
        .child(super::separator())
        .child(
            tools::button("videos", bar.videos.clone(), false)
                .on_click(tools::on(overlay, |o, cx| o.browse(Folder::Videos, cx))),
        );
    let column = div()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .children(bar.notice.map(notice))
        .child(row);
    let place = div()
        .absolute()
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(column);
    slide(place, Edge::Bottom, EDGE_GAP, bar.leaving, "action-bar")
}

fn capture_label(bar: &ActionBar<'_>) -> String {
    match bar.mode {
        Mode::Screens => format!("Capture {}  ⏎", bar.ready),
        Mode::Region => "Capture  ⏎".into(),
    }
}

fn setting_buttons(overlay: &Entity<Overlay>, settings: Settings) -> [Stateful<Div>; 5] {
    [
        setting_button(overlay, "fps", settings.fps, |s: &mut Settings| &mut s.fps),
        setting_button(overlay, "resolution", settings.resolution, |s: &mut Settings| {
            &mut s.resolution
        }),
        setting_button(overlay, "format", settings.format, |s: &mut Settings| {
            &mut s.format
        }),
        setting_button(overlay, "quality", settings.quality, |s: &mut Settings| {
            &mut s.quality
        }),
        setting_button(overlay, "audio", settings.audio, |s: &mut Settings| &mut s.audio),
    ]
}

/// A button showing a setting; a click moves it to its next value.
fn setting_button<T: Choice>(
    overlay: &Entity<Overlay>,
    id: &'static str,
    value: T,
    field: fn(&mut Settings) -> &mut T,
) -> Stateful<Div> {
    tools::button(id, value.label(), false)
        .on_click(tools::on(overlay, move |o, cx| o.cycle_setting(field, cx)))
}

/// Why the selection can't be recorded.
fn notice(text: &SharedString) -> Div {
    div()
        .px_2()
        .rounded_sm()
        .bg(rgba(0x000000c0))
        .text_sm()
        .text_color(rgba(0xff8a80ff))
        .child(text.clone())
}

/// `place` at `offset` from its screen `edge`; while `leaving`, it slides off past that edge.
pub fn slide(place: Div, edge: Edge, offset: f32, leaving: bool, id: &'static str) -> AnyElement {
    let at = move |place: Div, travelled: f32| match edge {
        Edge::Top => place.top(px(offset - travelled)),
        Edge::Bottom => place.bottom(px(offset - travelled)),
    };
    if !leaving {
        return at(place, 0.).into_any_element();
    }
    let animation = Animation::new(SLIDE).with_easing(ease_in_out);
    let animated = at(place, 0.).with_animation(id, animation, move |place, delta| {
        at(place, SLIDE_DISTANCE * delta)
    });
    animated.into_any_element()
}

impl Overlay {
    /// Record the selection (or, `everything`, all screens): the bars slide away, the overlay
    /// closes, and the recording starts with the drawings kept on screen.
    pub(super) fn record(&mut self, everything: bool, cx: &mut Context<Self>) {
        if !self.selecting() {
            return;
        }
        self.notice = None;
        let Some(target) = self.recording_target(everything) else {
            return;
        };
        self.record_target(target, cx);
    }

    /// Record `target`, unless it can't be: then say why and stay.
    pub(super) fn record_target(&mut self, target: Target, cx: &mut Context<Self>) {
        if let Err(problem) = self.recordable(&target) {
            self.notice = Some(problem);
            cx.notify();
            return;
        }
        self.phase = Phase::Leaving;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SLIDE).await;
            let _ = this.update(cx, |o, cx| o.launch_recording(target, cx));
        })
        .detach();
    }

    fn recording_target(&self, everything: bool) -> Option<Target> {
        if everything {
            return Some(Target::All);
        }
        let screens = self.selection.monitors().len();
        self.selection
            .confirm()
            .map(|request| record_target(request, screens))
    }

    /// Whether `target` can be recorded as things stand (e.g. not across screens yet).
    fn recordable(&self, target: &Target) -> Result<(), SharedString> {
        let monitors = self.selection.monitors();
        let checked = plan::plan(target, self.config.recording.record, monitors, None, None);
        checked.map(drop).map_err(|e| format!("{e:#}").into())
    }

    fn launch_recording(&mut self, target: Target, cx: &mut Context<Self>) {
        // Something else ended the overlay while the bars slid away.
        if !matches!(self.phase, Phase::Leaving) {
            return;
        }
        let command = Command::Start(target, Overrides::from(self.config.recording.record));
        let drawings = self.annotations.shapes().cloned().collect();
        self.phase = Phase::Done;
        super::GATE.leave(std::time::Instant::now());
        let entity = cx.entity();
        cx.defer(move |cx| super::close_windows(&entity, cx));
        if self.resident {
            super::launch::through_daemon(command, drawings, self.client.take(), cx);
        } else {
            super::launch::in_this_process(command, drawings, super::report_standalone_recording, cx);
        }
    }

    /// Move one recording setting to its next value, and remember it.
    pub(super) fn cycle_setting<T: Choice>(
        &mut self,
        field: fn(&mut Settings) -> &mut T,
        cx: &mut Context<Self>,
    ) {
        let value = field(&mut self.config.recording.record);
        *value = value.next();
        let saved = crate::config::remember(&self.config.recording);
        super::report_unremembered(saved, "the recording settings");
        cx.notify();
    }
}

/// What recording the selection means. Picking every screen is "all".
pub fn record_target(request: Request, screen_count: usize) -> Target {
    match request {
        Request::Region(rect) => Target::Region(rect),
        Request::Screens(names) if names.len() == screen_count => Target::All,
        Request::Screens(names) => Target::Screens(names),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::Rect;

    #[test]
    fn a_region_records_that_region() {
        let rect = Rect::new(1, 2, 300, 400);
        assert_eq!(record_target(Request::Region(rect), 3), Target::Region(rect));
    }

    #[test]
    fn some_screens_record_those_screens() {
        let picked = vec!["DP-1".to_string()];
        assert_eq!(
            record_target(Request::Screens(picked.clone()), 3),
            Target::Screens(picked)
        );
    }

    #[test]
    fn every_screen_records_all() {
        let picked = vec!["DP-1".to_string(), "DP-2".to_string()];
        assert_eq!(record_target(Request::Screens(picked), 2), Target::All);
    }
}
