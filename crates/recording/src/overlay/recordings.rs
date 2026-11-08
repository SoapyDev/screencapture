//! The recording feature in the overlay: V records the selection (Shift+V every screen), with
//! the recording settings and the videos' folder in the action bar.

use std::path::PathBuf;

use gpui::{AnyElement, App, Div, Keystroke, SharedString, Stateful, prelude::*};

use screencapture_core::{
    overlay::{
        feature::{Act, Actions, Ctx, Effect, Feature, HandOver},
        folder, mark_saved, report_unremembered, separator, widgets,
    },
    selection::{Request, Selection},
};

use crate::{
    command::Command,
    plan::{self, Target},
    settings::{Choice, Overrides, Recording, Settings},
};

pub struct Recordings {
    /// Where videos go and how they are made, as remembered.
    recording: Recording,
}

impl Recordings {
    #[must_use]
    pub const fn new(recording: Recording) -> Self {
        Self { recording }
    }

    /// Record the selection (or, `everything`, all screens): the bars slide away, the overlay
    /// closes, and the recording starts with the drawings kept on screen.
    fn record(&self, everything: bool, selection: &Selection) -> Effect {
        let target = if everything {
            Some(Target::All)
        } else {
            let screens = selection.monitors().len();
            selection.confirm().map(|request| record_target(request, screens))
        };
        target.map_or(Effect::Repaint, |target| self.record_target(target, selection))
    }

    /// Record `target`, unless it can't be: then say why and stay.
    fn record_target(&self, target: Target, selection: &Selection) -> Effect {
        let settings = self.recording.record;
        if let Err(problem) = plan::plan(&target, settings, selection.monitors(), None, None) {
            return Effect::Notice(format!("{problem:#}").into());
        }
        let command = Command::Start(target, Overrides::from(settings));
        Effect::HandOver(Box::new(move |hand, cx| launch(command, hand, cx)))
    }

    /// Move one recording setting to its next value, and remember it.
    fn cycle_setting<T: Choice>(&mut self, field: fn(&mut Settings) -> &mut T) -> Effect {
        let value = field(&mut self.recording.record);
        *value = value.next();
        let saved = screencapture_core::config::remember(&self.recording);
        report_unremembered(saved, "the recording settings");
        Effect::Repaint
    }

    fn browse(&self) -> Effect {
        folder::browse("videos", &self.recording.dir(None), Self::chosen)
    }

    fn chosen(&mut self, dir: PathBuf) {
        self.recording.video_dir = Some(dir);
        report_unremembered(
            screencapture_core::config::remember(&self.recording),
            "the folder",
        );
    }

    /// What a recording would be, for the button: format, resolution and rate.
    fn summary(&self) -> String {
        let settings = self.recording.record;
        let (format, resolution, fps) = (
            settings.format.name(),
            settings.resolution.name(),
            settings.fps.name(),
        );
        format!("{format} · {resolution} · {fps} fps")
    }

    fn folder_label(&self) -> String {
        format!("Videos in {}  ⇧O", folder::shown(&self.recording.dir(None)))
    }

    fn setting_buttons(&self, act: &Act) -> [Stateful<Div>; 5] {
        let settings = self.recording.record;
        [
            setting_button(act, "fps", settings.fps, |s| &mut s.fps),
            setting_button(act, "resolution", settings.resolution, |s| &mut s.resolution),
            setting_button(act, "format", settings.format, |s| &mut s.format),
            setting_button(act, "quality", settings.quality, |s| &mut s.quality),
            setting_button(act, "audio", settings.audio, |s| &mut s.audio),
        ]
    }
}

/// Start `command` once the overlay is gone: through the daemon that opened it, or here.
fn launch(command: Command, hand: HandOver, cx: &mut App) {
    if hand.resident {
        super::launch::through_daemon(command, hand.drawings, hand.client, cx);
    } else {
        super::launch::in_this_process(command, hand.drawings, report_standalone, cx);
    }
}

/// The end of a recording started from a standalone overlay: like a saved capture, print it.
fn report_standalone(outcome: anyhow::Result<crate::Outcome>) {
    match outcome {
        Ok(crate::Outcome::Saved(path)) => {
            mark_saved();
            println!("{}", path.display());
        }
        Ok(crate::Outcome::Failed(e)) => eprintln!("screencapture: {e}"),
        Ok(other) => eprintln!("screencapture: recording: {other:?}"),
        Err(e) => eprintln!("screencapture: {e:#}"),
    }
}

/// A button showing a setting; a click moves it to its next value.
fn setting_button<T: Choice>(
    act: &Act,
    id: &'static str,
    value: T,
    field: fn(&mut Settings) -> &mut T,
) -> Stateful<Div> {
    let cycle = act.on(move |me: &mut Recordings, _| me.cycle_setting(field));
    widgets::button(id, value.label(), false).on_click(cycle)
}

/// What recording the selection means. Picking every screen is "all".
pub fn record_target(request: Request, screen_count: usize) -> Target {
    match request {
        Request::Region(rect) => Target::Region(rect),
        Request::Screens(names) if names.len() == screen_count => Target::All,
        Request::Screens(names) => Target::Screens(names),
    }
}

impl Feature for Recordings {
    fn has_drawer(&self) -> bool {
        true
    }

    fn key(&mut self, key: &Keystroke, ctx: &Ctx) -> Option<Effect> {
        let shift = key.modifiers.shift;
        match key.key.as_str() {
            "v" => Some(self.record(shift, ctx.selection)),
            "o" if shift => Some(self.browse()),
            _ => None,
        }
    }

    fn actions(&self, act: &Act, _: &Ctx) -> Actions {
        let start = act.on(|me: &mut Self, ctx| me.record(false, ctx.selection));
        let hint = format!("Record  V  ·  {}", self.summary());
        let record = widgets::round_button("record", widgets::Glyph::RecordDot, hint, false);
        let record = record.on_click(start);
        let videos = widgets::button("videos", SharedString::from(self.folder_label()), false);
        let videos = videos.on_click(act.on(|me: &mut Self, _| me.browse()));
        let settings = self
            .setting_buttons(act)
            .into_iter()
            .map(IntoElement::into_any_element);
        Actions {
            main: vec![record.into_any_element()],
            drawer: settings
                .chain([separator().into_any_element(), videos.into_any_element()])
                .collect::<Vec<AnyElement>>(),
        }
    }

    /// Record the region the test selected.
    #[cfg(debug_assertions)]
    fn test_hook(&mut self, region: screencapture_core::capture::Rect) -> Option<Effect> {
        std::env::var_os("SCREENCAPTURE_E2E_RECORD")?;
        let command = Command::Start(Target::Region(region), Overrides::from(self.recording.record));
        Some(Effect::HandOver(Box::new(move |hand, cx| {
            launch(command, hand, cx);
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use screencapture_core::capture::Rect;

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

    #[test]
    fn the_record_button_summarises_format_resolution_and_rate() {
        let summary = Recordings::new(Recording::default()).summary();
        assert_eq!(summary, "MP4 · AV1 · Native · Native fps");
    }

    #[test]
    fn the_videos_button_shows_its_key() {
        let recordings = Recordings::new(Recording {
            video_dir: Some("/clips".into()),
            ..Recording::default()
        });
        assert_eq!(recordings.folder_label(), "Videos in /clips  ⇧O");
    }
}
