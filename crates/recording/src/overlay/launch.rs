//! Starting a recording from the overlay, and showing the recording frame while a recording
//! runs in this process.

use std::sync::{Arc, Mutex, PoisonError};

use futures::StreamExt;
use gpui::{App, AppContext, Global};

use screencapture_core::{
    daemon::{self, Reply, Request},
    drawings::Shared,
    overlay::gate::SETTLE,
};

use super::frame::{self as recording, Shown, Spec};
use crate::{OnScreen, command::Command, foreground::ShowFrame};

/// Drawings made in the overlay, waiting for the frame of the recording it started (UI thread).
#[derive(Default)]
struct FrameDrawings(Option<Shared>);

impl Global for FrameDrawings {}

/// The drawings for the frame now opening; there are none for recordings not started from the
/// overlay.
pub fn take_drawings(cx: &mut App) -> Option<Shared> {
    if !cx.has_global::<FrameDrawings>() {
        return None;
    }
    cx.remove_global::<FrameDrawings>().0
}

/// The frame's spec for `frame` (from the recorder), with the drawings waiting for it.
pub fn spec_for(frame: Option<OnScreen>, cx: &mut App) -> Option<Spec> {
    Some(Spec {
        on_screen: frame?,
        drawings: take_drawings(cx),
    })
}

/// The daemon's recording frame, while one is on screen (UI thread).
#[derive(Default)]
struct ShownFrame(Option<Shown>);

impl Global for ShownFrame {}

/// Show the frame for `frame` (from the daemon's recorder), replacing any other; `None` takes
/// it down.
pub fn show_frame(frame: Option<OnScreen>, cx: &mut App) {
    let spec = spec_for(frame, cx);
    let mut shown = cx.default_global::<ShownFrame>().0.take();
    replace_frame(&mut shown, spec, cx);
    cx.set_global(ShownFrame(shown));
}

/// Replace the frame on screen with `spec`'s (none: take it down).
pub fn replace_frame(shown: &mut Option<Shown>, spec: Option<Spec>, cx: &mut App) {
    if let Some(old) = shown.take() {
        recording::hide(old, cx);
    }
    *shown = spec.map(|spec| recording::show(&spec, cx));
}

/// Hand `command` to the daemon once the overlay is off the screens, and pass its answer to
/// `client` (the Print that opened the overlay).
pub fn through_daemon(
    command: Command,
    drawings: Option<Shared>,
    client: Option<daemon::Client>,
    cx: &mut App,
) {
    cx.set_global(FrameDrawings(drawings));
    cx.spawn(async move |cx| {
        // Until the overlay is off the screens.
        cx.background_executor().timer(SETTLE).await;
        let request: crate::Request = Request::Feature(command);
        let answer = cx
            .background_spawn(async move { daemon::send(&daemon::socket_path(), &request) })
            .await;
        let answer = answer_of(answer);
        if !matches!(answer.reply, Reply::Recording(_)) {
            cx.update(take_drawings);
        }
        // The notes too: what the recording had to adjust.
        if let Some(client) = client {
            client.pass_on(&answer);
        }
    })
    .detach();
}

fn answer_of(answer: Option<anyhow::Result<daemon::Answer>>) -> daemon::Answer {
    let failed = |message: String| daemon::Answer {
        reply: Reply::Error(message),
        notes: Vec::new(),
    };
    match answer {
        Some(Ok(answer)) => answer,
        Some(Err(e)) => failed(format!("{e:#}")),
        None => failed("the daemon went away".into()),
    }
}

/// Run `work` on its own thread while this app shows the frames it asks for, then quit.
/// `drawings` go with its first frame.
pub fn drive_frames(drawings: Option<Shared>, work: impl FnOnce(ShowFrame) + Send + 'static, cx: &mut App) {
    cx.set_global(FrameDrawings(drawings));
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    std::thread::spawn(move || {
        // Owned only here, to cross to the UI thread.
        work(&mut |frame| drop(tx.unbounded_send(Work::Show(frame.cloned()))));
        let _ = tx.unbounded_send(Work::Done);
    });
    cx.spawn(async move |cx| {
        let mut shown = None;
        while let Some(work) = rx.next().await {
            cx.update(|cx| work.show(&mut shown, cx));
        }
    })
    .detach();
}

/// What the work on its own thread asks of the UI thread.
enum Work {
    /// Show this frame (or take it down).
    Show(Option<OnScreen>),
    /// The work is done: take the frame down, and quit.
    Done,
}

impl Work {
    fn show(self, shown: &mut Option<Shown>, cx: &mut App) {
        match self {
            Self::Show(frame) => {
                let spec = spec_for(frame, cx);
                replace_frame(shown, spec, cx);
            }
            Self::Done => {
                replace_frame(shown, None, cx);
                cx.quit();
            }
        }
    }
}

/// Run `work` (a recording in this process) with its frame on screen; returns what it returned.
///
/// # Panics
///
/// When `work` panicked: that panic is passed on.
pub fn with_recording_frame<R: Send + 'static>(work: impl FnOnce(ShowFrame) -> R + Send + 'static) -> R {
    let result = Arc::new(Mutex::new(None));
    let slot = result.clone();
    gpui_platform::application().run(move |cx: &mut App| {
        cx.set_quit_mode(gpui::QuitMode::Explicit);
        drive_frames(
            None,
            move |show| *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(work(show)),
            cx,
        );
    });
    let done = result.lock().unwrap_or_else(PoisonError::into_inner).take();
    done.expect("the recording thread finished before gpui quit")
}

/// Record `command` in this process (no daemon): once the overlay is off the screens, this
/// process takes the daemon's place on the socket until `record stop`; `report` gets the end.
pub fn in_this_process(
    command: Command,
    drawings: Option<Shared>,
    report: impl FnOnce(anyhow::Result<crate::Outcome>) + Send + 'static,
    cx: &mut App,
) {
    let work = move |show: ShowFrame| {
        std::thread::sleep(SETTLE);
        report(crate::foreground::run(command, &daemon::socket_path(), show));
    };
    drive_frames(drawings, work, cx);
}
