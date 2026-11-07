//! Drawings made over the screens, as the features that keep them see them: a screenshot burns
//! them in, a recording keeps them on screen. The editing feature makes them; without it there
//! are none.

use std::sync::Arc;

use gpui::{AnyView, App};
use image::RgbaImage;

use crate::capture::Rect;

pub trait Drawings: Send + Sync {
    /// Burn them into `image`, whose top-left is desktop point `origin`, at `scale` image pixels
    /// per desktop pixel, only inside `clip` (left, top, right, bottom, in image pixels).
    fn rasterize(&self, image: &mut RgbaImage, origin: (f32, f32), scale: f32, clip: [u32; 4]);
    #[cfg_attr(
        not(all(target_os = "linux", feature = "wayland")),
        expect(dead_code, reason = "only the recording frame, which needs layer-shell, asks")
    )]
    /// The room each one takes on the desktop.
    fn extents(&self) -> Vec<Rect>;
    #[cfg_attr(
        not(all(target_os = "linux", feature = "wayland")),
        expect(dead_code, reason = "only the recording frame, which needs layer-shell, asks")
    )]
    /// A view of those inside `area` (desktop pixels), for a window covering it.
    fn view(&self, area: Rect, cx: &mut App) -> AnyView;
}

/// Drawings handed from one feature to another (and across threads).
pub type Shared = Arc<dyn Drawings>;
