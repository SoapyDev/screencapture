//! The controls while recording, on screen: a thin strip at the top of every screen, and the
//! drawer it opens (Stop, Discard, time recorded). Mouse only: the keyboard stays with the
//! user's apps; key bindings go through `screencapture record stop|discard` instead.

use std::{cell::Cell, rc::Rc, time::Instant};

use gpui::{
    Animation, AnimationExt, AnyWindowHandle, App, AppContext, Context, Entity, SharedString, Window, div,
    ease_in_out, prelude::*, px, rgba,
};

use super::{
    drawer::{self, Change, Drawer},
    recording, tools,
};
use crate::{
    capture::MonitorInfo,
    daemon::{self, Request},
    record::{OnScreen, command::Command},
};

/// What the strips and the drawer share.
pub struct Controls {
    drawer: Drawer,
    /// Where the pointer last reached the top: the drawer opens there.
    screen: Option<MonitorInfo>,
    /// The drawer's window while open (also closed with the recording's other windows).
    window: Rc<Cell<Option<AnyWindowHandle>>>,
    monitors: Vec<MonitorInfo>,
    started: Instant,
    summary: SharedString,
}

/// One strip per screen; `window` will hold the drawer while it is open.
pub fn open_strips(
    on_screen: &OnScreen,
    window: Rc<Cell<Option<AnyWindowHandle>>>,
    cx: &mut App,
) -> Vec<AnyWindowHandle> {
    let controls = cx.new(|_| Controls {
        drawer: Drawer::Closed,
        screen: None,
        window,
        monitors: on_screen.monitors.clone(),
        started: Instant::now(),
        summary: on_screen.summary.clone().into(),
    });
    let strips = on_screen
        .monitors
        .iter()
        .filter_map(|screen| open_strip(&controls, screen, cx));
    strips.collect()
}

fn open_strip(controls: &Entity<Controls>, screen: &MonitorInfo, cx: &mut App) -> Option<AnyWindowHandle> {
    let monitors = controls.read(cx).monitors.clone();
    let display = recording::display_of(screen, &monitors, cx)?;
    let options = recording::layer_options(drawer::hot_strip(screen.rect), screen.rect, display);
    let (controls, screen) = (controls.clone(), screen.clone());
    let opened = cx.open_window(options, |_, cx| cx.new(|_| StripView { controls, screen }));
    opened.map(Into::into).ok()
}

impl Controls {
    fn pointer_entered(&mut self, screen: Option<&MonitorInfo>, cx: &Context<Self>) {
        let before = self.drawer;
        self.drawer = self.drawer.pointer_entered(Instant::now());
        if self.drawer == before {
            return;
        }
        if let Some(screen) = screen {
            self.screen = Some(screen.clone());
        }
        Self::check_after(drawer::DWELL, cx);
    }

    fn pointer_left(&mut self, cx: &Context<Self>) {
        let before = self.drawer;
        self.drawer = self.drawer.pointer_left(Instant::now());
        if self.drawer != before {
            Self::check_after(drawer::LINGER, cx);
        }
    }

    /// Look again once `wait` is over: the pointer may have rested or stayed away long enough.
    fn check_after(wait: std::time::Duration, cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, Self::check);
        })
        .detach();
    }

    fn check(&mut self, cx: &mut Context<Self>) {
        let (drawer, change) = self.drawer.at(Instant::now());
        self.drawer = drawer;
        match change {
            Change::Open => self.open_drawer(cx),
            Change::Close => self.close_drawer(cx),
            Change::Nothing => {}
        }
    }

    fn open_drawer(&self, cx: &mut Context<Self>) {
        let Some(screen) = self.screen.as_ref() else {
            return;
        };
        let Some(display) = recording::display_of(screen, &self.monitors, cx) else {
            return;
        };
        let options = recording::layer_options(drawer::drawer_place(screen.rect), screen.rect, display);
        let (controls, window) = (cx.entity(), self.window.clone());
        // After this update: the new window renders at once, and reads the controls.
        cx.defer(move |cx| {
            let opened = cx.open_window(options, |_, cx| cx.new(|cx| DrawerView::new(controls, cx)));
            window.set(opened.map(Into::into).ok());
        });
    }

    fn close_drawer(&self, cx: &mut App) {
        let Some(window) = self.window.take() else { return };
        // Deferred: this may run inside the drawer's own click, where it can't be removed.
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        });
    }

    /// Ask whoever records (the daemon, or this process standing in for it) to `command`.
    fn send(&mut self, command: Command, cx: &mut Context<Self>) {
        self.drawer = Drawer::Closed;
        self.close_drawer(cx);
        cx.background_spawn(async move { daemon::send(&daemon::socket_path(), &Request::Record(command)) })
            .detach();
    }
}

/// The strip at the top of one screen: only notices the pointer.
struct StripView {
    controls: Entity<Controls>,
    screen: MonitorInfo,
}

impl Render for StripView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(pointer_tracking(&self.controls, Some(&self.screen)))
    }
}

/// The drawer: time recorded, what is recorded, Stop, Discard.
struct DrawerView {
    controls: Entity<Controls>,
}

impl DrawerView {
    fn new(controls: Entity<Controls>, cx: &Context<Self>) -> Self {
        // The clock moves every second while the drawer is open.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        })
        .detach();
        Self { controls }
    }
}

impl Render for DrawerView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let controls = self.controls.read(cx);
        let clock = drawer::clock(controls.started.elapsed());
        let summary = controls.summary.clone();
        let panel = super::panel()
            .child(div().size(px(10.)).rounded_full().bg(rgba(0xff4d4dff)))
            .child(div().px_2().child(clock))
            .child(div().px_2().text_color(rgba(0xffffffa0)).child(summary))
            .child(super::separator())
            .child(command_button(&self.controls, "stop", "■ Stop", Command::Stop))
            .child(command_button(
                &self.controls,
                "discard",
                "✕ Discard",
                Command::Discard,
            ));
        let slide_in = Animation::new(super::actions::SLIDE).with_easing(ease_in_out);
        div()
            .id("drawer")
            .size_full()
            .flex()
            .justify_center()
            .child(panel.with_animation("drawer-in", slide_in, |panel, delta| {
                panel.mt(px(-56. * (1. - delta)))
            }))
            .child(pointer_tracking(&self.controls, None))
    }
}

/// Tells `controls` when the pointer moves in this window (on `screen`'s strip, or in the drawer)
/// and when it leaves. Window-wide: elements' own hover is blocked by the drawer's panel, and a
/// new window reports hover before the pointer has been near it.
fn pointer_tracking(controls: &Entity<Controls>, screen: Option<&MonitorInfo>) -> impl IntoElement {
    let (controls, screen) = (controls.clone(), screen.cloned());
    gpui::canvas(
        |_, _, _| (),
        move |_, (), window, _| {
            let (entering, leaving) = (controls.clone(), controls);
            window.on_mouse_event(move |_: &gpui::MouseMoveEvent, phase, _, cx| {
                if phase == gpui::DispatchPhase::Bubble {
                    entering.update(cx, |controls, cx| controls.pointer_entered(screen.as_ref(), cx));
                }
            });
            window.on_mouse_event(move |_: &gpui::MouseExitEvent, phase, _, cx| {
                if phase == gpui::DispatchPhase::Bubble {
                    leaving.update(cx, |controls, cx| controls.pointer_left(cx));
                }
            });
        },
    )
    .absolute()
    .size_full()
}

fn command_button(
    controls: &Entity<Controls>,
    id: &'static str,
    label: &'static str,
    command: Command,
) -> impl IntoElement {
    let controls = controls.clone();
    let label: SharedString = label.into();
    tools::button(id, label, false).on_click(move |_, _, cx| {
        let command = command.clone();
        controls.update(cx, |controls, cx| controls.send(command, cx));
    })
}
