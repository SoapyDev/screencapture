//! The editing feature's drawings, as other features see them (see [`screencapture_core::drawings`]).

use std::{collections::HashMap, sync::Arc};

use gpui::{
    AnyView, App, AppContext, Context, ImageSource, ObjectFit, RenderImage, Window, canvas, div, img,
    prelude::*, px,
};
use image::RgbaImage;

use screencapture_core::{
    capture::Rect,
    drawings::{Drawings, Shared},
};

use super::tools;
use crate::annotate::{Shape, Tool};

/// Shapes drawn in the overlay, kept after it closes.
pub struct Drawn(Arc<[Shape]>);

impl Drawn {
    /// `shapes`, or nothing when there are none.
    pub fn of<'a>(shapes: impl Iterator<Item = &'a Shape>) -> Option<Shared> {
        let shapes: Arc<[Shape]> = shapes.cloned().collect();
        (!shapes.is_empty()).then(|| Arc::new(Self(shapes)) as Shared)
    }
}

impl Drawings for Drawn {
    fn rasterize(&self, image: &mut RgbaImage, origin: (f32, f32), scale: f32, clip: [u32; 4]) {
        crate::annotate::rasterize_clipped(image, &self.0, origin, scale, clip);
    }

    fn extents(&self) -> Vec<Rect> {
        self.0.iter().map(extent).collect()
    }

    fn view(&self, area: Rect, cx: &mut App) -> AnyView {
        let shapes = self.0.clone();
        cx.new(|_| DrawnView {
            area,
            shapes,
            texts: HashMap::new(),
        })
        .into()
    }
}

/// Room a drawing takes on screen: its points, plus what its stroke, arrow head or disc adds.
fn extent(shape: &Shape) -> Rect {
    let (left, top, right, bottom) = shape.bounds();
    let pad = if shape.tool == Tool::Text {
        0.
    } else {
        shape.width.mul_add(4., 4.)
    };
    Rect::new(
        (left - pad).floor() as i32,
        (top - pad).floor() as i32,
        2.0f32.mul_add(pad, right - left).ceil() as i32,
        2.0f32.mul_add(pad, bottom - top).ceil() as i32,
    )
}

/// The drawings inside `area`, on a window covering it.
struct DrawnView {
    area: Rect,
    shapes: Arc<[Shape]>,
    /// Text boxes rendered once, per size on screen.
    texts: HashMap<tools::TextKey, Arc<RenderImage>>,
}

impl DrawnView {
    /// A text box, as an image placed where it lies in `area`.
    fn text(&mut self, shape: &Shape, scale: f32) -> Option<gpui::AnyElement> {
        let room = extent(shape);
        room.intersect(self.area)?;
        let key = tools::TextKey::new(shape, scale);
        let image = self
            .texts
            .entry(key)
            .or_insert_with_key(tools::TextKey::render)
            .clone();
        let placed = img(ImageSource::Render(image))
            .absolute()
            .left(px((room.x - self.area.x) as f32))
            .top(px((room.y - self.area.y) as f32))
            .w(px(room.w as f32))
            .h(px(room.h as f32))
            .object_fit(ObjectFit::Fill);
        Some(placed.into_any_element())
    }

    /// Any other drawing, painted over the whole window.
    fn painted(&self, shape: &Shape) -> gpui::AnyElement {
        let (shape, origin) = (shape.clone(), (self.area.x, self.area.y));
        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                tools::paint_shapes(std::slice::from_ref(&shape), origin, bounds, window);
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }
}

impl Render for DrawnView {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let scale = window.scale_factor();
        let shapes = self.shapes.clone();
        let drawn = shapes.iter().filter_map(|shape| match shape.tool {
            Tool::Text => self.text(shape, scale),
            _ => Some(self.painted(shape)),
        });
        let drawn: Vec<_> = drawn.collect();
        div().size_full().relative().children(drawn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line() -> Shape {
        Shape {
            tool: Tool::Line,
            points: vec![(10., 20.), (30., 20.)],
            color: [255, 0, 0, 255],
            width: 2.,
            ..Shape::default()
        }
    }

    #[test]
    fn no_shapes_are_no_drawings() {
        assert!(Drawn::of(std::iter::empty()).is_none());
    }

    #[test]
    fn a_stroke_takes_room_beyond_its_points() {
        // 2 px wide: 12 px of room around the points, for the stroke and its caps.
        assert_eq!(extent(&line()), Rect::new(-2, 8, 44, 24));
    }

    #[test]
    fn the_drawings_burn_into_an_image() {
        let drawn = Drawn::of([line()].iter()).expect("one shape is a drawing");
        let mut image = RgbaImage::new(40, 40);
        drawn.rasterize(&mut image, (0., 0.), 1., [0, 0, 40, 40]);
        assert_ne!(image.get_pixel(20, 20).0, [0; 4]);
    }
}
