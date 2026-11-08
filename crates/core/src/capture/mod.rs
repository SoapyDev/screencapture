//! Screen capture: grab every monitor once into a [`Snapshot`], then cut regions or stitch
//! screens out of it.
//!
//! All geometry is in the compositor's logical (global desktop) coordinates;
//! images are in physical pixels, in BGRA byte order (what compositors hand out and gpui shows),
//! so the only RGBA conversion is of the final cut, just before it is encoded.

#[cfg(all(target_os = "linux", feature = "wayland"))]
pub mod pixels;
#[cfg(all(target_os = "linux", feature = "wayland"))]
mod wlr;
#[cfg(any(not(target_os = "linux"), feature = "x11"))]
mod xcap_backend;

use std::ops::Deref;

use anyhow::{Result, bail};
use image::{ImageBuffer, Rgba, RgbaImage, imageops};

/// Swap the first and third channel of every pixel: BGRA ⇄ RGBA. Big images (whole screens)
/// are done in parallel bands.
pub fn swap_red_blue(raw: &mut [u8]) {
    const BAND: usize = 1 << 20; // bytes; a multiple of 4 so bands split between pixels
    if raw.len() <= BAND {
        swap_band(raw);
        return;
    }
    std::thread::scope(|scope| {
        for band in raw.chunks_mut(raw.len().div_ceil(16).next_multiple_of(4).max(BAND)) {
            scope.spawn(|| swap_band(band));
        }
    });
}

fn swap_band(raw: &mut [u8]) {
    for px in raw.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
}

/// Axis-aligned rectangle in logical desktop coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    #[must_use]
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// Rectangle spanning two corner points, in any order.
    #[must_use]
    pub fn from_corners((x0, y0): (i32, i32), (x1, y1): (i32, i32)) -> Self {
        let (left, right) = (x0.min(x1), x0.max(x1));
        let (top, bottom) = (y0.min(y1), y0.max(y1));
        Self::new(left, top, right - left, bottom - top)
    }

    #[must_use]
    pub const fn right(self) -> i32 {
        self.x + self.w
    }

    #[must_use]
    pub const fn bottom(self) -> i32 {
        self.y + self.h
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    #[must_use]
    pub const fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    #[must_use]
    pub fn intersect(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = Self::new(
            x,
            y,
            self.right().min(other.right()) - x,
            self.bottom().min(other.bottom()) - y,
        );
        (!r.is_empty()).then_some(r)
    }

    #[must_use]
    pub fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self::new(
            x,
            y,
            self.right().max(other.right()) - x,
            self.bottom().max(other.bottom()) - y,
        )
    }

    /// Parse slurp-style geometry: `X,Y WxH`.
    ///
    /// # Errors
    ///
    /// When `geometry` is not `X,Y WxH`, or its size is empty.
    pub fn parse(geometry: &str) -> Result<Self> {
        let parse = || -> Option<Self> {
            let (position, size) = geometry.trim().split_once(' ')?;
            let (left, top) = position.split_once(',')?;
            let (width, height) = size.trim().split_once('x')?;
            Some(Self::new(
                left.parse().ok()?,
                top.parse().ok()?,
                width.parse().ok()?,
                height.parse().ok()?,
            ))
        };
        match parse() {
            Some(rect) if !rect.is_empty() => Ok(rect),
            _ => bail!("invalid geometry {geometry:?}, expected \"X,Y WxH\" (e.g. \"100,200 640x480\")"),
        }
    }
}

/// A monitor's identity and place on the desktop, known without capturing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorInfo {
    pub name: String,
    /// Logical position and size on the desktop.
    pub rect: Rect,
    /// Position the windowing system reports for the output (`wl_output.geometry` on Wayland),
    /// which is what gpui uses as the display origin. Used to match gpui displays to monitors.
    pub output_position: (i32, i32),
    /// Refresh rate in mHz, when the windowing system says.
    pub refresh_mhz: Option<u32>,
    /// Size in physical pixels, upright: what recording all of it encodes.
    pub pixels: PixelSize,
}

impl MonitorInfo {
    /// Physical pixels per logical pixel (its scale); 1 when either size is unknown.
    #[must_use]
    pub fn density(&self) -> f64 {
        if self.rect.w <= 0 || self.pixels.width == 0 {
            return 1.;
        }
        f64::from(self.pixels.width) / f64::from(self.rect.w)
    }
}

/// A size in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelSize {
    pub width: u32,
    pub height: u32,
}

impl PixelSize {
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// `rect`'s size at one physical pixel per logical pixel (negative sides count as 0).
    #[must_use]
    pub const fn of(rect: Rect) -> Self {
        Self::new(crate::units::unsigned(rect.w), crate::units::unsigned(rect.h))
    }
}

/// Native Wayland capture, unless `SCREENCAPTURE_BACKEND=xcap` forces the portable one.
#[cfg(all(target_os = "linux", feature = "wayland"))]
fn on_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        && (cfg!(not(feature = "x11"))
            || std::env::var("SCREENCAPTURE_BACKEND").map_or(true, |b| b != "xcap"))
}

/// Run the native Wayland backend when it applies, else the xcap one, whichever are compiled in.
macro_rules! dispatch {
    ($wlr:expr, $xcap:expr) => {{
        #[cfg(all(target_os = "linux", feature = "wayland"))]
        if on_wayland() {
            #[cfg(not(feature = "x11"))]
            return $wlr;
            #[cfg(feature = "x11")]
            match $wlr {
                Ok(v) => return Ok(v),
                Err(e) => eprintln!("wlr-screencopy unavailable ({e:#}); falling back to xcap"),
            }
        }
        #[cfg(any(not(target_os = "linux"), feature = "x11"))]
        return $xcap;
        #[allow(unreachable_code)]
        Err(anyhow::anyhow!(
            "no capture backend for this session (built without the `x11` feature, and not on Wayland)"
        ))
    }};
}

/// List monitors without capturing them.
///
/// # Errors
///
/// When the display server can't be reached or won't list its screens.
pub fn monitors() -> Result<Vec<MonitorInfo>> {
    dispatch!(wlr::monitors(), xcap_backend::monitors())
}

fn capture_shots(wanted: &dyn Fn(&MonitorInfo) -> bool) -> Result<Vec<Shot>> {
    // Built without a backend (a feature crate's own tests): nothing asks.
    #[cfg(all(target_os = "linux", not(any(feature = "wayland", feature = "x11"))))]
    let _ = wanted;
    dispatch!(wlr::capture(wanted), xcap_backend::capture(wanted))
}

/// Captures that keep what can be kept between them: on Wayland, the screencopy connection and
/// its already-faulted buffers (about 10 ms faster per capture). Used by the daemon.
#[derive(Default)]
pub struct Session {
    #[cfg(all(target_os = "linux", feature = "wayland"))]
    wlr: Option<wlr::Capturer>,
}

impl Session {
    /// Every monitor, frozen at once. Same backend choice (and xcap fallback) as one-off captures.
    ///
    /// # Errors
    ///
    /// When the display server refuses the capture, or there is no screen.
    // Without Wayland there is nothing to keep between captures.
    #[cfg_attr(
        not(all(target_os = "linux", feature = "wayland")),
        allow(clippy::unused_self, clippy::needless_pass_by_ref_mut)
    )]
    pub fn take(&mut self) -> Result<Snapshot> {
        dispatch!(self.take_wlr(), Snapshot::new(xcap_backend::capture(&|_| true)?))
    }

    /// Give back what is kept for speed (on Wayland, ~15 MB of buffers per screen), keeping
    /// the connection. For long idle spells.
    #[cfg_attr(
        not(all(target_os = "linux", feature = "wayland")),
        allow(
            clippy::unused_self,
            clippy::needless_pass_by_ref_mut,
            clippy::missing_const_for_fn
        )
    )]
    pub fn release_buffers(&mut self) {
        #[cfg(all(target_os = "linux", feature = "wayland"))]
        if let Some(capturer) = &mut self.wlr {
            capturer.drop_buffers();
        }
    }

    #[cfg(all(target_os = "linux", feature = "wayland"))]
    fn take_wlr(&mut self) -> Result<Snapshot> {
        // Twice at most: a hotplug invalidates the kept connection once.
        for _ in 0..2 {
            if self.wlr.is_none() {
                self.wlr = Some(wlr::Capturer::new()?);
            }
            let capturer = self.wlr.as_mut().expect("just set");
            match capturer.capture(&|_| true) {
                Ok(shots) => return Snapshot::new(shots),
                Err(e) if e.is::<wlr::OutputsChanged>() => self.wlr = None,
                Err(e) => {
                    self.wlr = None;
                    return Err(e);
                }
            }
        }
        Snapshot::new(wlr::capture(&|_| true)?)
    }
}

/// One monitor's frozen contents.
pub struct Shot<C = Vec<u8>> {
    pub name: String,
    /// Logical position and size on the desktop.
    pub rect: Rect,
    /// Where the windowing system places the output (see [`MonitorInfo::output_position`]).
    pub output_position: (i32, i32),
    /// Physical pixels (BGRA), already rotated to match the logical orientation. Owned, or
    /// borrowed from the overlay's display copy so saving needs no second copy.
    pub image: ImageBuffer<Rgba<u8>, C>,
}

impl<C: Deref<Target = [u8]>> Shot<C> {
    pub fn info(&self) -> MonitorInfo {
        MonitorInfo {
            name: self.name.clone(),
            rect: self.rect,
            output_position: self.output_position,
            refresh_mhz: None,
            pixels: PixelSize::new(self.image.width(), self.image.height()),
        }
    }

    /// Physical pixels per logical pixel.
    pub fn scale(&self) -> f64 {
        f64::from(self.image.width()) / f64::from(self.rect.w.max(1))
    }
}

/// Every monitor, captured at the same moment.
pub struct Snapshot<C = Vec<u8>> {
    pub shots: Vec<Shot<C>>,
}

/// Where part of the desktop lands in an output image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// Source area, global logical coordinates.
    pub src: Rect,
    /// Its top-left in the output, logical coordinates.
    pub dst: (i32, i32),
}

struct Piece<'a, C> {
    shot: &'a Shot<C>,
    /// Source area, in global logical coordinates (inside `shot.rect`).
    src: Rect,
    /// Destination top-left, in output logical coordinates.
    dst: (i32, i32),
}

impl Snapshot {
    /// Capture only the monitors `wanted` accepts; skipping the rest saves time.
    ///
    /// # Errors
    ///
    /// When the display server refuses the capture, or `wanted` accepts no screen.
    pub fn take_where(wanted: impl Fn(&MonitorInfo) -> bool) -> Result<Self> {
        Self::new(capture_shots(&wanted)?)
    }

    /// Every monitor, frozen at once.
    ///
    /// # Errors
    ///
    /// When the display server refuses the capture, or there is no screen.
    pub fn take() -> Result<Self> {
        Self::take_where(|_| true)
    }
}

impl<C: Deref<Target = [u8]>> Snapshot<C> {
    /// The screens of `shots`, in layout order.
    ///
    /// # Errors
    ///
    /// When `shots` is empty.
    pub fn new(mut shots: Vec<Shot<C>>) -> Result<Self> {
        if shots.is_empty() {
            bail!("no monitors found");
        }
        shots.sort_by_key(|s| (s.rect.x, s.rect.y));
        Ok(Self { shots })
    }

    /// The image `request` asks for.
    ///
    /// # Errors
    ///
    /// When the region touches no screen, or a named screen doesn't exist.
    pub fn grab(&self, request: &crate::selection::Request) -> Result<RgbaImage> {
        use crate::selection::Request;
        match request {
            Request::Region(rect) => self.crop(*rect),
            Request::Screens(names) if names.is_empty() => self.all(),
            Request::Screens(names) => self.stitch(names),
        }
    }

    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Shot<C>> {
        self.shots.iter().find(|s| s.name.eq_ignore_ascii_case(name))
    }

    /// Cut an arbitrary desktop region. Parts outside every monitor are transparent.
    ///
    /// # Errors
    ///
    /// When `region` touches no screen.
    pub fn crop(&self, region: Rect) -> Result<RgbaImage> {
        let pieces = self.crop_pieces(region)?;
        Ok(render(&pieces, region.w, region.h))
    }

    fn crop_pieces(&self, region: Rect) -> Result<Vec<Piece<'_, C>>> {
        let pieces: Vec<Piece<C>> = self
            .shots
            .iter()
            .filter_map(|shot| {
                let src = shot.rect.intersect(region)?;
                Some(Piece {
                    shot,
                    src,
                    dst: (src.x - region.x, src.y - region.y),
                })
            })
            .collect();
        if pieces.is_empty() {
            bail!("region {region:?} does not touch any monitor");
        }
        Ok(pieces)
    }

    /// Stitch the named monitors into one contiguous image. Monitors keep their relative
    /// layout, but empty bands between them (e.g. an unselected screen in the middle) are
    /// removed so the result has no gaps.
    ///
    /// # Errors
    ///
    /// When a name matches no screen, or `names` is empty.
    pub fn stitch(&self, names: &[String]) -> Result<RgbaImage> {
        let (pieces, w, h) = self.stitch_pieces(names)?;
        Ok(render(&pieces, w, h))
    }

    fn stitch_pieces(&self, names: &[String]) -> Result<(Vec<Piece<'_, C>>, i32, i32)> {
        let selected = self.selected(names)?;
        let xs = Compressor::new(selected.iter().map(|s| (s.rect.x, s.rect.right())));
        let ys = Compressor::new(selected.iter().map(|s| (s.rect.y, s.rect.bottom())));
        let pieces = selected.into_iter().map(|shot| Piece {
            shot,
            src: shot.rect,
            dst: (xs.map(shot.rect.x), ys.map(shot.rect.y)),
        });
        let pieces = pieces.collect();
        Ok((pieces, xs.extent(), ys.extent()))
    }

    /// The screens called `names`, each once, in that order: an unknown name, or none, is an
    /// error.
    fn selected(&self, names: &[String]) -> Result<Vec<&Shot<C>>> {
        let mut selected: Vec<&Shot<C>> = Vec::new();
        for name in names {
            let Some(shot) = self.find(name) else {
                bail!(
                    "unknown monitor {name:?} (available: {})",
                    self.names().join(", ")
                );
            };
            if !selected.iter().any(|s| std::ptr::eq(*s, shot)) {
                selected.push(shot);
            }
        }
        if selected.is_empty() {
            bail!("no monitor selected");
        }
        Ok(selected)
    }

    /// How [`Self::grab`] lays out the desktop for `request`, and its pixels per logical pixel.
    ///
    /// # Errors
    ///
    /// When the region touches no screen, or a named screen doesn't exist.
    pub fn placements(&self, request: &crate::selection::Request) -> Result<(Vec<Placement>, f64)> {
        use crate::selection::Request;
        let pieces = match request {
            Request::Region(rect) => self.crop_pieces(*rect)?,
            Request::Screens(names) if names.is_empty() => self.stitch_pieces(&self.names())?.0,
            Request::Screens(names) => self.stitch_pieces(names)?.0,
        };
        let scale = output_scale(&pieces);
        Ok((
            pieces
                .iter()
                .map(|p| Placement {
                    src: p.src,
                    dst: p.dst,
                })
                .collect(),
            scale,
        ))
    }

    /// Every screen stitched into one image.
    ///
    /// # Errors
    ///
    /// None in practice: a snapshot holds at least one screen, each found by its own name.
    pub fn all(&self) -> Result<RgbaImage> {
        self.stitch(&self.names())
    }

    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.shots.iter().map(|s| s.name.clone()).collect()
    }
}

/// Removes the uncovered gaps along one axis.
struct Compressor {
    /// Merged covered intervals, sorted, as (start, end, offset in output).
    spans: Vec<(i32, i32, i32)>,
}

impl Compressor {
    fn new(intervals: impl Iterator<Item = (i32, i32)>) -> Self {
        let mut intervals: Vec<_> = intervals.collect();
        intervals.sort_unstable();
        let mut merged: Vec<(i32, i32)> = Vec::new();
        for (start, end) in intervals {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        let mut offset = 0;
        let spans = merged
            .into_iter()
            .map(|(start, end)| {
                let span = (start, end, offset);
                offset += end - start;
                span
            })
            .collect();
        Self { spans }
    }

    fn map(&self, v: i32) -> i32 {
        let &(start, _, offset) = self
            .spans
            .iter()
            .rev()
            .find(|(start, _, _)| *start <= v)
            .expect("value comes from a covered interval");
        offset + v - start
    }

    fn extent(&self) -> i32 {
        self.spans
            .last()
            .map_or(0, |&(start, end, offset)| offset + end - start)
    }
}

/// Output density: the highest among the screens involved, so no monitor loses detail.
fn output_scale<C: Deref<Target = [u8]>>(pieces: &[Piece<C>]) -> f64 {
    pieces.iter().map(|p| p.shot.scale()).fold(1.0_f64, f64::max)
}

/// Draw pieces onto a transparent canvas of `w`x`h` logical pixels, at [`output_scale`].
fn render<C: Deref<Target = [u8]>>(pieces: &[Piece<C>], w: i32, h: i32) -> RgbaImage {
    let scale = output_scale(pieces);
    let px = |v: i32| crate::units::saturating_u32((f64::from(v) * scale).round());
    let mut canvas = RgbaImage::new(px(w), px(h));
    for piece in pieces {
        let (dx, dy) = (i64::from(px(piece.dst.0)), i64::from(px(piece.dst.1)));
        let cut = piece.cut();
        let (dst_w, dst_h) = (px(piece.src.w), px(piece.src.h));
        if image::GenericImageView::dimensions(&*cut) == (dst_w, dst_h) {
            imageops::replace(&mut canvas, &*cut, dx, dy);
        } else {
            let scaled = imageops::resize(&*cut, dst_w, dst_h, imageops::FilterType::Lanczos3);
            imageops::replace(&mut canvas, &scaled, dx, dy);
        }
    }
    canvas
}

impl<C: Deref<Target = [u8]>> Piece<'_, C> {
    /// This piece of its screen's image, in that screen's pixels.
    fn cut(&self) -> image::SubImage<&ImageBuffer<Rgba<u8>, C>> {
        let scale = self.shot.scale();
        let px = |v: i32| crate::units::saturating_u32((f64::from(v) * scale).round());
        let (x, y) = (
            px(self.src.x - self.shot.rect.x),
            px(self.src.y - self.shot.rect.y),
        );
        let w = px(self.src.w).min(self.shot.image.width() - x);
        let h = px(self.src.h).min(self.shot.image.height() - y);
        imageops::crop_imm(&self.shot.image, x, y, w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn shot(name: &str, rect: Rect, scale: u32, color: u8) -> Shot {
        let image = RgbaImage::from_pixel(
            crate::units::unsigned(rect.w) * scale,
            crate::units::unsigned(rect.h) * scale,
            Rgba([color, 0, 0, 255]),
        );
        Shot {
            name: name.into(),
            rect,
            output_position: (rect.x * 2, rect.y),
            image,
        }
    }

    fn monitor_of(width: i32, pixels: PixelSize) -> MonitorInfo {
        MonitorInfo {
            name: "A".into(),
            rect: Rect::new(0, 0, width, 1440),
            output_position: (0, 0),
            refresh_mhz: None,
            pixels,
        }
    }

    #[test]
    fn density_is_pixels_per_logical_pixel() {
        let monitor = monitor_of(2560, PixelSize::new(5120, 2880));
        assert!(
            (monitor.density() - 2.).abs() < f64::EPSILON,
            "{}",
            monitor.density()
        );
    }

    #[test]
    fn density_without_pixels_is_one() {
        let monitor = monitor_of(2560, PixelSize::new(0, 0));
        assert!(
            (monitor.density() - 1.).abs() < f64::EPSILON,
            "{}",
            monitor.density()
        );
    }

    #[test]
    fn a_shot_knows_its_monitors_pixels() {
        let info = shot("A", Rect::new(0, 0, 100, 50), 2, 1).info();
        assert_eq!(info.pixels, PixelSize::new(200, 100));
    }

    #[test]
    fn snapshot_knows_its_monitors_in_layout_order() {
        let snap = Snapshot::new(vec![
            shot("B", Rect::new(100, 0, 100, 50), 1, 2),
            shot("A", Rect::new(0, 0, 100, 50), 1, 1),
        ])
        .unwrap();
        assert_eq!(
            snap.shots.iter().map(Shot::info).collect::<Vec<_>>(),
            vec![
                MonitorInfo {
                    name: "A".into(),
                    rect: Rect::new(0, 0, 100, 50),
                    output_position: (0, 0),
                    refresh_mhz: None,
                    pixels: PixelSize::new(100, 50),
                },
                MonitorInfo {
                    name: "B".into(),
                    rect: Rect::new(100, 0, 100, 50),
                    output_position: (200, 0),
                    refresh_mhz: None,
                    pixels: PixelSize::new(100, 50),
                },
            ]
        );
    }

    #[test]
    fn grab_answers_each_kind_of_request() {
        use crate::selection::Request;
        let snap = three_screens();
        assert_eq!(
            snap.grab(&Request::Region(Rect::new(5, 5, 10, 10)))
                .unwrap()
                .dimensions(),
            (10, 10)
        );
        assert_eq!(
            snap.grab(&Request::Screens(vec![])).unwrap().dimensions(),
            (300, 50)
        );
        assert_eq!(
            snap.grab(&Request::Screens(vec!["B".into()]))
                .unwrap()
                .dimensions(),
            (100, 50)
        );
    }

    #[test]
    fn a_snapshot_can_borrow_its_pixels() {
        let owned = three_screens();
        let borrowed = Snapshot::new(
            owned
                .shots
                .iter()
                .map(|s| Shot {
                    name: s.name.clone(),
                    rect: s.rect,
                    output_position: s.output_position,
                    image: ImageBuffer::from_raw(
                        s.image.width(),
                        s.image.height(),
                        s.image.as_raw().as_slice(),
                    )
                    .unwrap(),
                })
                .collect(),
        )
        .unwrap();
        let region = Rect::new(90, 10, 20, 5);
        assert_eq!(borrowed.crop(region).unwrap(), owned.crop(region).unwrap());
        assert_eq!(borrowed.all().unwrap(), owned.all().unwrap());
    }

    fn three_screens() -> Snapshot {
        Snapshot::new(vec![
            shot("A", Rect::new(0, 0, 100, 50), 1, 1),
            shot("B", Rect::new(100, 0, 100, 50), 1, 2),
            shot("C", Rect::new(200, 0, 100, 50), 1, 3),
        ])
        .unwrap()
    }

    #[test]
    fn stitch_removes_gap_between_non_adjacent_screens() {
        let img = three_screens().stitch(&["A".into(), "C".into()]).unwrap();
        assert_eq!(img.dimensions(), (200, 50));
        assert_eq!(img.get_pixel(99, 0)[0], 1);
        assert_eq!(img.get_pixel(100, 0)[0], 3);
    }

    #[test]
    fn all_screens_are_contiguous() {
        let img = three_screens().all().unwrap();
        assert_eq!(img.dimensions(), (300, 50));
        assert_eq!(img.get_pixel(150, 10)[0], 2);
    }

    #[test]
    fn crop_spans_monitors() {
        let img = three_screens().crop(Rect::new(90, 10, 20, 5)).unwrap();
        assert_eq!(img.dimensions(), (20, 5));
        assert_eq!(img.get_pixel(9, 0)[0], 1);
        assert_eq!(img.get_pixel(10, 0)[0], 2);
    }

    #[test]
    fn crop_outside_monitors_is_transparent() {
        let snap = Snapshot::new(vec![
            shot("A", Rect::new(0, 0, 100, 100), 1, 1),
            shot("B", Rect::new(100, 50, 100, 100), 1, 2),
        ])
        .unwrap();
        let img = snap.crop(Rect::new(50, 0, 100, 100)).unwrap();
        assert_eq!(img.get_pixel(75, 10)[3], 0);
        assert_eq!(img.get_pixel(75, 60)[0], 2);
    }

    #[test]
    fn mixed_scale_renders_at_highest_density() {
        let snap = Snapshot::new(vec![
            shot("A", Rect::new(0, 0, 100, 50), 1, 1),
            shot("B", Rect::new(100, 0, 100, 50), 2, 2),
        ])
        .unwrap();
        let img = snap.all().unwrap();
        assert_eq!(img.dimensions(), (400, 100));
        assert_eq!(img.get_pixel(150, 50)[0], 1);
        assert_eq!(img.get_pixel(250, 50)[0], 2);
    }

    #[test]
    fn parse_geometry() {
        assert_eq!(
            Rect::parse("10,-20 300x400").unwrap(),
            Rect::new(10, -20, 300, 400)
        );
        assert!(Rect::parse("10,20 0x5").is_err());
        assert!(Rect::parse("garbage").is_err());
    }

    #[test]
    fn unknown_monitor_is_an_error() {
        assert!(three_screens().stitch(&["Z".into()]).is_err());
    }

    #[test]
    fn swapping_big_and_small_buffers() {
        let mut small = vec![1, 2, 3, 4, 5, 6, 7, 8];
        swap_red_blue(&mut small);
        assert_eq!(small, vec![3, 2, 1, 4, 7, 6, 5, 8]);
        // Big enough for bands, and not a multiple of their size.
        let original: Vec<u8> = (0..(3 << 20) + 4 * 7)
            .map(|i| u8::try_from(i % 253).expect("a remainder of 253 fits a byte"))
            .collect();
        let mut big = original.clone();
        swap_red_blue(&mut big);
        for (a, b) in big.as_chunks::<4>().0.iter().zip(original.as_chunks::<4>().0) {
            assert_eq!(*a, [b[2], b[1], b[0], b[3]]);
        }
    }
}
