//! Starting a recording from the overlay, and showing the recording frame while a recording
//! runs in this process.

use std::sync::{Arc, Mutex, PoisonError};

use futures::StreamExt;
use gpui::{App, AppContext, Global};

use super::recording::{self, Shown, Spec};
use crate::{
    daemon::{self, Reply, Request},
    drawings::Shared,
    record::{OnScreen, command::Command, foreground::ShowFrame},
};

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
        cx.background_executor().timer(super::gate::SETTLE).await;
        let request: crate::record::Request = Request::Feature(command);
        let answer = cx
            .background_spawn(async move { daemon::send(&daemon::socket_path(), &request) })
            .await;
        let reply = reply_of(answer);
        if !matches!(reply, Reply::Recording(_)) {
            cx.update(take_drawings);
        }
        if let Some(client) = client {
            client.reply(&reply);
        }
    })
    .detach();
}

fn reply_of(answer: Option<anyhow::Result<daemon::Answer>>) -> Reply {
    match answer {
        Some(Ok(answer)) => answer.reply,
        Some(Err(e)) => Reply::Error(format!("{e:#}")),
        None => Reply::Error("the daemon went away".into()),
    }
}

/// Run `work` on its own thread while this app shows the frames it asks for, then quit.
/// `drawings` go with its first frame.
pub fn drive_frames(drawings: Option<Shared>, work: impl FnOnce(ShowFrame) + Send + 'static, cx: &mut App) {
    cx.set_global(FrameDrawings(drawings));
    // `Some(frame)`: show it (or take it down); `None`: the work is done.
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    std::thread::spawn(move || {
        work(&mut |frame| {
            // Owned only here, to cross to the UI thread.
            let _ = tx.unbounded_send(Some(frame.cloned()));
        });
        let _ = tx.unbounded_send(None);
    });
    cx.spawn(async move |cx| {
        let mut shown = None;
        while let Some(Some(frame)) = rx.next().await {
            cx.update(|cx| {
                let spec = spec_for(frame, cx);
                replace_frame(&mut shown, spec, cx);
            });
        }
        cx.update(|cx| {
            replace_frame(&mut shown, None, cx);
            cx.quit();
        });
    })
    .detach();
}

/// Run `work` (a recording in this process) with its frame on screen; returns what it returned.
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
    report: impl FnOnce(anyhow::Result<crate::record::Outcome>) + Send + 'static,
    cx: &mut App,
) {
    let work = move |show: ShowFrame| {
        std::thread::sleep(super::gate::SETTLE);
        report(crate::record::foreground::run(
            command,
            &daemon::socket_path(),
            show,
        ));
    };
    drive_frames(drawings, work, cx);
}
