//! Test driver: real pointer input through `zwlr_virtual_pointer_v1`, for checking the overlay
//! the way a user drives it.
//!
//! `cargo run --example drive -- W H drag X1 Y1 X2 Y2` drags with the left button across a
//! W×H layout (all monitors), in layout pixels.

use std::time::Duration;

use wayland_client::{
    Connection, Dispatch, QueueHandle,
    protocol::{wl_pointer, wl_registry, wl_seat},
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

#[derive(Default)]
struct State {
    seat: Option<wl_seat::WlSeat>,
    manager: Option<ZwlrVirtualPointerManagerV1>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        s: &mut Self,
        r: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, .. } = e {
            match interface.as_str() {
                "wl_seat" => s.seat = Some(r.bind(name, 1, qh, ())),
                "zwlr_virtual_pointer_manager_v1" => s.manager = Some(r.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

wayland_client::delegate_noop!(State: ignore wl_seat::WlSeat);
wayland_client::delegate_noop!(State: ZwlrVirtualPointerManagerV1);
wayland_client::delegate_noop!(State: ZwlrVirtualPointerV1);

const BTN_LEFT: u32 = 0x110;

fn main() {
    let args: Vec<u32> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let [w, h, x1, y1, x2, y2] = args[..] else {
        panic!("usage: drive W H drag X1 Y1 X2 Y2");
    };
    let conn = Connection::connect_to_env().expect("wayland");
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
        .expect("no zwlr_virtual_pointer_manager_v1");
    let pointer = manager.create_virtual_pointer(state.seat.as_ref(), &qh, ());
    let mut time = 0;
    let mut step = |f: &dyn Fn(u32)| {
        time += 16;
        f(time);
        pointer.frame();
        conn.flush().expect("the compositor connection stays open");
        std::thread::sleep(Duration::from_millis(16));
    };
    step(&|t| pointer.motion_absolute(t, x1, y1, w, h));
    step(&|t| pointer.button(t, BTN_LEFT, wl_pointer::ButtonState::Pressed));
    let steps = 30;
    for i in 1..=steps {
        let x = x1 + (x2 - x1) * i / steps;
        let y = y1 + (y2 - y1) * i / steps;
        step(&|t| pointer.motion_absolute(t, x, y, w, h));
    }
    step(&|t| pointer.button(t, BTN_LEFT, wl_pointer::ButtonState::Released));
    pointer.destroy();
    queue
        .roundtrip(&mut state)
        .expect("the compositor takes the last events");
}
