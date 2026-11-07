//! Real pointer input through `zwlr_virtual_pointer_v1`, for driving the program the way a user
//! does: moves the compositor's own pointer, so every surface sees genuine enter, motion, leave
//! and button events. Shared by the end-to-end tests and the `drive` example.

use std::time::Duration;

use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    protocol::{wl_pointer, wl_registry, wl_seat},
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

/// A place on the desktop, in layout pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spot {
    pub x: u32,
    pub y: u32,
}

#[derive(Default)]
struct State {
    seat: Option<wl_seat::WlSeat>,
    manager: Option<ZwlrVirtualPointerManagerV1>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, .. } = event {
            match interface.as_str() {
                "wl_seat" => state.seat = Some(registry.bind(name, 1, qh, ())),
                "zwlr_virtual_pointer_manager_v1" => state.manager = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

wayland_client::delegate_noop!(State: ignore wl_seat::WlSeat);
wayland_client::delegate_noop!(State: ZwlrVirtualPointerManagerV1);
wayland_client::delegate_noop!(State: ZwlrVirtualPointerV1);

const BTN_LEFT: u32 = 0x110;
/// Moves per glide: enough for every surface on the way to see the pointer.
const GLIDE_STEPS: i64 = 30;

/// One virtual pointer, alive until dropped (keep one for a whole gesture: a new device makes
/// surfaces forget where the pointer is).
pub struct Pointer {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    device: ZwlrVirtualPointerV1,
    /// The whole desktop's size: absolute moves are relative to it.
    layout: (u32, u32),
    time: u32,
}

impl Pointer {
    pub fn new(layout: (u32, u32)) -> Self {
        let conn = Connection::connect_to_env().expect("tests run inside a Wayland session");
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut state = State::default();
        queue
            .roundtrip(&mut state)
            .expect("the compositor answers the registry");
        let manager = state
            .manager
            .as_ref()
            .expect("the compositor offers virtual pointers (wlroots)");
        let pointer = manager.create_virtual_pointer(state.seat.as_ref(), &qh, ());
        Self {
            conn,
            queue,
            state,
            device: pointer,
            layout,
            time: 0,
        }
    }

    /// One event, then a frame's pause, like a real device.
    fn step(&mut self, event: impl FnOnce(&ZwlrVirtualPointerV1, u32)) {
        self.time += 16;
        event(&self.device, self.time);
        self.device.frame();
        self.conn.flush().expect("the compositor connection stays open");
        std::thread::sleep(Duration::from_millis(16));
    }

    pub fn jump(&mut self, to: Spot) {
        let (width, height) = self.layout;
        self.step(|pointer, time| pointer.motion_absolute(time, to.x, to.y, width, height));
    }

    /// From `from` to `to` in small moves.
    pub fn glide(&mut self, from: Spot, to: Spot) {
        let along =
            |a: u32, b: u32, i: i64| (i64::from(a) + (i64::from(b) - i64::from(a)) * i / GLIDE_STEPS) as u32;
        for i in 0..=GLIDE_STEPS {
            self.jump(Spot {
                x: along(from.x, to.x, i),
                y: along(from.y, to.y, i),
            });
        }
    }

    pub fn press(&mut self) {
        self.step(|pointer, time| pointer.button(time, BTN_LEFT, wl_pointer::ButtonState::Pressed));
    }

    pub fn release(&mut self) {
        self.step(|pointer, time| pointer.button(time, BTN_LEFT, wl_pointer::ButtonState::Released));
    }

    pub fn click(&mut self) {
        self.press();
        self.release();
    }
}

impl Drop for Pointer {
    fn drop(&mut self) {
        self.device.destroy();
        let _ = self.queue.roundtrip(&mut self.state);
    }
}
