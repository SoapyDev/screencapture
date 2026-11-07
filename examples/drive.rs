//! Test driver: real pointer input, for checking the program the way a user drives it.
//!
//! `cargo run --example drive -- W H drag X1 Y1 X2 Y2` drags with the left button across a
//! W×H layout (all monitors), in layout pixels; `glide` instead of `drag` moves the same way
//! without the button, `click` glides then clicks at the end, and `W H move X Y` jumps the
//! pointer there.

#[path = "../tests/support/pointer.rs"]
mod pointer;

use pointer::{Pointer, Spot};

fn main() {
    let words: Vec<String> = std::env::args().skip(1).collect();
    let numbers: Vec<u32> = words.iter().filter_map(|word| word.parse().ok()).collect();
    let gesture = words
        .iter()
        .find(|word| word.parse::<u32>().is_err())
        .map_or("move", String::as_str);
    match (gesture, &numbers[..]) {
        ("move", &[w, h, x, y]) => Pointer::new((w, h)).jump(Spot { x, y }),
        (gesture, &[w, h, x1, y1, x2, y2]) => {
            run(gesture, (w, h), Spot { x: x1, y: y1 }, Spot { x: x2, y: y2 });
        }
        _ => panic!("usage: drive W H drag|glide|click X1 Y1 X2 Y2 | drive W H move X Y"),
    }
}

fn run(gesture: &str, layout: (u32, u32), from: Spot, to: Spot) {
    let mut pointer = Pointer::new(layout);
    pointer.jump(from);
    match gesture {
        "drag" => {
            pointer.press();
            pointer.glide(from, to);
            pointer.release();
        }
        "glide" => pointer.glide(from, to),
        "click" => {
            pointer.glide(from, to);
            pointer.click();
        }
        other => panic!("unknown gesture {other:?}: drag, glide, click or move"),
    }
}
