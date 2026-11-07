//! Recording in the daemon: `record …` requests, a recorder that may quit by itself, and the
//! frame on screen while it records.

use std::time::Duration;

use super::{
    launch,
    service::{Service, Ui},
};
use crate::{
    daemon::{Client, Reply},
    record::{LiveEnv, Outcome, Recorder, command::Command},
};

/// How often the daemon checks on a running recorder (it may quit by itself).
const WATCH_RECORDER: Duration = Duration::from_millis(250);

#[derive(Default)]
pub struct Recording {
    recorder: Recorder,
}

impl Service for Recording {
    type Request = Command;

    /// The frame changes at once; the answer may wait for the file to be finished (joining
    /// screens is slow), on its own thread, so other requests don't.
    fn handle(&mut self, command: Command, client: Client, overlay_open: bool, ui: &Ui) {
        // Never record the frozen overlay.
        if command.starts() && self.recorder.recording().is_none() && overlay_open {
            client.reply(&Reply::Busy);
            return;
        }
        let before = self.recorder.recording().map(std::path::Path::to_path_buf);
        let outcome = self.recorder.run(command, &mut LiveEnv);
        // A different recording (or none) now: its frame replaces the old one.
        if self.recorder.recording() != before.as_deref() {
            self.show_frame(ui);
        }
        answer(outcome, client);
    }

    /// While recording: notice a recorder that quit by itself, and take its frame down.
    fn watch(&mut self, ui: &Ui) -> Option<Duration> {
        self.recorder.recording()?;
        let Some(outcome) = self.recorder.ended_by_itself() else {
            return Some(WATCH_RECORDER);
        };
        self.show_frame(ui);
        std::thread::spawn(move || {
            let outcome = outcome.finished();
            eprintln!("screencapture: the recorder stopped by itself: {outcome:?}");
        });
        None
    }

    /// Print while recording: say so, and leave the screen alone.
    fn instead_of_overlay(&self) -> Option<Reply> {
        let path = self.recorder.recording()?;
        Some(Reply::Recording(path.to_path_buf()))
    }
}

impl Recording {
    /// Show the recorder's frame now (none: take it down).
    fn show_frame(&self, ui: &Ui) {
        // Owned only here, to cross to the UI thread.
        let frame = self.recorder.frame().cloned();
        ui.run(move |cx| launch::show_frame(frame, cx));
    }
}

/// Answer `client` now, or once the file is finished, on its own thread.
fn answer(outcome: Outcome, client: Client) {
    if matches!(outcome, Outcome::Finishing(_)) {
        std::thread::spawn(move || outcome.answer(client));
    } else {
        outcome.answer(client);
    }
}
