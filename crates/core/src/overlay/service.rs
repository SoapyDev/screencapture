//! A feature serving daemon requests of its own beside the overlay (recording).

use std::time::Duration;

use gpui::App;

use crate::daemon::{Client, Extension, NoExtension, Reply};

/// Work for the UI thread.
pub type UiJob = Box<dyn FnOnce(&mut App) + Send>;

/// Hands work from the daemon's request thread to the UI thread.
#[derive(Clone)]
pub struct Ui(pub(super) futures::channel::mpsc::UnboundedSender<super::ToUi>);

impl Ui {
    /// Run `job` on the UI thread, soon.
    pub fn run(&self, job: impl FnOnce(&mut App) + Send + 'static) {
        let _ = self.0.unbounded_send(super::ToUi::Run(Box::new(job)));
    }
}

/// What the daemon serves besides the overlay.
pub trait Service: Send + 'static {
    /// Its requests (see [`Extension`]).
    type Request: Extension + Send;

    /// One of its requests, on the daemon's request thread; `overlay_open` while an overlay is
    /// on the screens.
    fn handle(&mut self, request: Self::Request, client: Client, overlay_open: bool, ui: &Ui);

    /// Before every wait: when to look again (`None`: only after the next request).
    fn watch(&mut self, _ui: &Ui) -> Option<Duration> {
        None
    }

    /// The answer to an overlay request instead of opening the overlay (while recording).
    fn instead_of_overlay(&self) -> Option<Reply> {
        None
    }
}

/// Nothing besides the overlay.
pub struct NoService;

impl Service for NoService {
    type Request = NoExtension;

    fn handle(&mut self, request: NoExtension, _: Client, _: bool, _: &Ui) {
        let () = request.absurd();
    }
}
