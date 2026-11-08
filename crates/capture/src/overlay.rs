//! The capture feature in the overlay: Enter (or Capture) saves the selection as a screenshot.

use std::path::PathBuf;

use gpui::{Keystroke, prelude::*};

use screencapture_core::{
    overlay::{
        feature::{Act, Actions, Ctx, Effect, Feature, SaveJob},
        folder, report_unremembered, widgets,
    },
    selection::{Mode, Request},
    trace,
};

use crate::{
    job,
    save::{self, Screenshots},
};

pub struct Capturing {
    /// Where screenshots go, as remembered.
    screenshots: Screenshots,
}

impl Capturing {
    #[must_use]
    pub const fn new(screenshots: Screenshots) -> Self {
        Self { screenshots }
    }

    /// Save `request`, if there is one yet.
    fn save(&self, request: Option<Request>) -> Effect {
        request.map_or(Effect::Repaint, |request| {
            Effect::Save(request, save_job(self.screenshots.dir(None)))
        })
    }

    fn browse(&self) -> Effect {
        folder::browse("screenshots", &self.screenshots.dir(None), Self::chosen)
    }

    fn chosen(&mut self, dir: PathBuf) {
        self.screenshots.output_dir = Some(dir);
        report_unremembered(
            screencapture_core::config::remember(&self.screenshots),
            "the folder",
        );
    }

    fn folder_label(&self) -> String {
        format!("Save in {}  O", folder::shown(&self.screenshots.dir(None)))
    }
}

/// Compose the screenshot, save it into `dir`, and put it on the clipboard.
fn save_job(dir: PathBuf) -> SaveJob {
    Box::new(move |snapshot, request, drawings| {
        let image = job::compose(snapshot, request, drawings)?;
        trace("composed");
        let saved = save::save_png(&image, &dir, chrono::Local::now())?;
        trace("saved");
        screencapture_core::clipboard::copy_png(saved.png);
        trace("copied");
        Ok(saved.path)
    })
}

/// The Capture button: how many screens it would save.
fn capture_label(ctx: &Ctx) -> (String, bool) {
    let ready = match ctx.selection.confirm() {
        Some(Request::Screens(names)) => names.len(),
        Some(Request::Region(_)) => 1,
        None => 0,
    };
    let label = match ctx.selection.mode {
        Mode::Screens => format!("Capture {ready}  ⏎"),
        Mode::Region => "Capture  ⏎".into(),
    };
    (label, ready > 0)
}

impl Feature for Capturing {
    fn key(&mut self, key: &Keystroke, ctx: &Ctx) -> Option<Effect> {
        match key.key.as_str() {
            "enter" => Some(self.save(ctx.selection.confirm())),
            "o" if !key.modifiers.shift => Some(self.browse()),
            _ => None,
        }
    }

    fn actions(&self, act: &Act, ctx: &Ctx) -> Actions {
        let (label, ready) = capture_label(ctx);
        let capture = widgets::button("capture", label, ready);
        let capture = capture.on_click(act.on(|me: &mut Self, ctx| me.save(ctx.selection.confirm())));
        let folder = widgets::button("folder", self.folder_label(), false);
        let folder = folder.on_click(act.on(|me: &mut Self, _| me.browse()));
        Actions {
            main: vec![capture.into_any_element()],
            drawer: vec![folder.into_any_element()],
        }
    }

    /// Save the region the test selected, unless it is recorded instead (or also clicked
    /// while the recording starts).
    #[cfg(debug_assertions)]
    fn test_hook(&mut self, region: screencapture_core::capture::Rect) -> Option<Effect> {
        let recorded = std::env::var_os("SCREENCAPTURE_E2E_RECORD").is_some();
        let clicked_too = std::env::var_os("SCREENCAPTURE_E2E_CAPTURE_TOO").is_some();
        (!recorded || clicked_too).then(|| self.save(Some(Request::Region(region))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_button_shows_its_key() {
        let capturing = Capturing::new(Screenshots {
            output_dir: Some("/shots".into()),
        });
        assert_eq!(capturing.folder_label(), "Save in /shots  O");
    }

    #[test]
    fn enter_with_nothing_selected_saves_nothing() {
        let capturing = Capturing::new(Screenshots::default());
        assert!(matches!(capturing.save(None), Effect::Repaint));
    }
}
