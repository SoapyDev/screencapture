//! Raw framebuffer to BGRA conversion, independent of any windowing system.
//!
//! BGRA (bytes B, G, R, A) is what gpui displays and what compositors usually hand out
//! (`XRGB8888`), so the common case is a straight copy that only sets alpha. Only the final,
//! cropped image is turned into RGBA for the PNG.

use image::RgbaImage;

#[cfg(test)]
use super::swap_red_blue;

/// Framebuffer formats we can read, named like `DRM/wl_shm` fourcc codes (little-endian words).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// Bytes B, G, R, X.
    Xrgb8888,
    /// Bytes B, G, R, A.
    Argb8888,
    /// Bytes R, G, B, X.
    Xbgr8888,
    /// Bytes R, G, B, A.
    Abgr8888,
    /// 32-bit word: 2 alpha | 10 red | 10 green | 10 blue (blue in the low bits).
    Xrgb2101010,
    Argb2101010,
    /// 32-bit word: 2 alpha | 10 blue | 10 green | 10 red (red in the low bits).
    Xbgr2101010,
    Abgr2101010,
}

impl PixelFormat {
    /// Lower is better: already BGRA beats a swap beats a 10-bit repack.
    #[must_use]
    pub const fn preference(self) -> u8 {
        match self {
            Self::Xrgb8888 | Self::Argb8888 => 0,
            Self::Xbgr8888 | Self::Abgr8888 => 1,
            Self::Xrgb2101010 | Self::Argb2101010 | Self::Xbgr2101010 | Self::Abgr2101010 => 2,
        }
    }
}

/// Output transform, as in `wl_output::transform`: how the compositor rotates (counter-clockwise)
/// and flips the framebuffer to show it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Transform {
    #[default]
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
    Flipped,
    Flipped90,
    Flipped180,
    Flipped270,
}

/// Convert a framebuffer to opaque BGRA (in an `RgbaImage` container: the channel order is
/// B, G, R, A). `y_invert` means rows are stored bottom-up.
#[must_use]
pub fn to_bgra(
    format: PixelFormat,
    data: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    y_invert: bool,
) -> RgbaImage {
    // A 2560×1440 screen is ~15 MB: in 8 bands (one per core, several screens at once) the
    // copy and the page faults of the new buffer go several times faster.
    let bands = if height >= 256 { 8 } else { 1 };
    to_bgra_banded(format, data, (width, height, stride), y_invert, bands)
}

/// [`to_bgra`] with the rows split into `bands` converted in parallel.
fn to_bgra_banded(
    format: PixelFormat,
    data: &[u8],
    (width, height, stride): (u32, u32, u32),
    y_invert: bool,
    bands: usize,
) -> RgbaImage {
    let row_bytes = width as usize * 4;
    let mut out = vec![0_u8; row_bytes * height as usize];
    let rows_per_band = (height as usize).div_ceil(bands.max(1)).max(1);
    let source = Source {
        format,
        data,
        row_bytes,
        height: height as usize,
        stride: stride as usize,
        y_invert,
    };
    if rows_per_band >= height as usize {
        source.convert(0, &mut out);
    } else {
        std::thread::scope(|scope| {
            for (band, chunk) in out.chunks_mut(rows_per_band * row_bytes).enumerate() {
                scope.spawn(move || source.convert(band * rows_per_band, chunk));
            }
        });
    }
    RgbaImage::from_raw(width, height, out).expect("buffer sized from dimensions")
}

/// The screen's pixels as the compositor gave them.
#[derive(Clone, Copy)]
struct Source<'a> {
    format: PixelFormat,
    data: &'a [u8],
    row_bytes: usize,
    height: usize,
    stride: usize,
    /// Rows come bottom first.
    y_invert: bool,
}

impl Source<'_> {
    /// Convert the rows from `first` on into `out`, as many as it holds.
    fn convert(self, first: usize, out: &mut [u8]) {
        for (i, dst) in out.chunks_exact_mut(self.row_bytes).enumerate() {
            let y = first + i;
            let src_y = if self.y_invert { self.height - 1 - y } else { y };
            let start = src_y * self.stride;
            convert_row(self.format, &self.data[start..start + self.row_bytes], dst);
        }
    }
}

fn convert_row(format: PixelFormat, src: &[u8], dst: &mut [u8]) {
    let pairs = src.as_chunks::<4>().0.iter().zip(dst.as_chunks_mut::<4>().0);
    match format {
        // Already B, G, R: copy, make opaque (X is undefined, A meaningless for a screen).
        PixelFormat::Xrgb8888 | PixelFormat::Argb8888 => {
            for (&[b, g, r, _], d) in pairs {
                *d = [b, g, r, 255];
            }
        }
        PixelFormat::Xbgr8888 | PixelFormat::Abgr8888 => {
            for (&[r, g, b, _], d) in pairs {
                *d = [b, g, r, 255];
            }
        }
        PixelFormat::Xrgb2101010 | PixelFormat::Argb2101010 => {
            for (s, d) in pairs {
                let w = u32::from_le_bytes(*s);
                *d = [ten_to_eight(w), ten_to_eight(w >> 10), ten_to_eight(w >> 20), 255];
            }
        }
        PixelFormat::Xbgr2101010 | PixelFormat::Abgr2101010 => {
            for (s, d) in pairs {
                let w = u32::from_le_bytes(*s);
                *d = [ten_to_eight(w >> 20), ten_to_eight(w >> 10), ten_to_eight(w), 255];
            }
        }
    }
}

const fn ten_to_eight(channel: u32) -> u8 {
    ((channel & 0x3FF) >> 2) as u8
}

/// Turn a framebuffer-oriented image into what the user sees on screen: undo the compositor's
/// work in reverse order, turning back clockwise, then mirroring.
#[must_use]
pub fn apply_transform(image: RgbaImage, transform: Transform) -> RgbaImage {
    use Transform::{Flipped, Flipped90, Flipped180, Flipped270, Normal, Rotate90, Rotate180, Rotate270};
    use image::imageops::{flip_horizontal, rotate90, rotate180, rotate270};
    // imageops rotates clockwise; wl_output transforms are counter-clockwise.
    let image = match transform {
        Normal | Flipped => image,
        Rotate90 | Flipped90 => rotate90(&image),
        Rotate180 | Flipped180 => rotate180(&image),
        Rotate270 | Flipped270 => rotate270(&image),
    };
    match transform {
        Flipped | Flipped90 | Flipped180 | Flipped270 => flip_horizontal(&image),
        _ => image,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two pixels per row, two rows, stride padded to 12 bytes.
    fn frame(px: [[u8; 4]; 4]) -> Vec<u8> {
        let mut data = Vec::new();
        for row in px.chunks(2) {
            for p in row {
                data.extend_from_slice(p);
            }
            data.extend_from_slice(&[0xEE; 4]);
        }
        data
    }

    #[test]
    fn xrgb_is_copied_with_opaque_alpha() {
        let data = frame([[1, 2, 3, 0], [4, 5, 6, 9], [7, 8, 9, 0], [10, 11, 12, 0]]);
        let img = to_bgra(PixelFormat::Xrgb8888, &data, 2, 2, 12, false);
        assert_eq!(
            img.as_raw(),
            &[1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
        );
    }

    #[test]
    fn xbgr_swaps_red_and_blue_and_forces_opaque() {
        let data = frame([[1, 2, 3, 9], [4, 5, 6, 9], [7, 8, 9, 9], [10, 11, 12, 9]]);
        let img = to_bgra(PixelFormat::Abgr8888, &data, 2, 2, 12, false);
        assert_eq!(
            img.as_raw(),
            &[3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255]
        );
    }

    #[test]
    fn swapping_red_and_blue_twice_is_identity() {
        let mut raw = vec![1, 2, 3, 4, 5, 6, 7, 8];
        swap_red_blue(&mut raw);
        assert_eq!(raw, vec![3, 2, 1, 4, 7, 6, 5, 8]);
        swap_red_blue(&mut raw);
        assert_eq!(raw, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn y_invert_flips_rows() {
        let data = frame([[1, 0, 0, 0], [2, 0, 0, 0], [3, 0, 0, 0], [4, 0, 0, 0]]);
        let img = to_bgra(PixelFormat::Xbgr8888, &data, 2, 2, 12, true);
        // Red sits in channel 2 of BGRA.
        assert_eq!(img.get_pixel(0, 0)[2], 3);
        assert_eq!(img.get_pixel(1, 1)[2], 2);
    }

    #[test]
    fn ten_bit_formats_keep_top_eight_bits() {
        // red = 0x3FF, green = 0x200, blue = 0x004
        let word: u32 = (0x3FF << 20) | (0x200 << 10) | 0x004;
        let data = word.to_le_bytes();
        let img = to_bgra(PixelFormat::Xrgb2101010, &data, 1, 1, 4, false);
        assert_eq!(img.get_pixel(0, 0).0, [0x01, 0x80, 0xFF, 255]);

        let word: u32 = (0x004 << 20) | (0x200 << 10) | 0x3FF;
        let img = to_bgra(PixelFormat::Xbgr2101010, &word.to_le_bytes(), 1, 1, 4, false);
        assert_eq!(img.get_pixel(0, 0).0, [0x01, 0x80, 0xFF, 255]);
    }

    /// 2x1 image: red then green.
    fn red_green() -> RgbaImage {
        RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 255]).unwrap()
    }

    /// What `wl_output.transform` says the compositor does to the picture to fill the
    /// framebuffer: mirror first (flipped variants), then turn counter-clockwise.
    fn as_compositor_draws(image: &RgbaImage, transform: Transform) -> RgbaImage {
        use Transform::{Flipped, Flipped90, Flipped180, Flipped270, Normal, Rotate90, Rotate180, Rotate270};
        use image::imageops::{flip_horizontal, rotate90, rotate180, rotate270};
        let image = match transform {
            Flipped | Flipped90 | Flipped180 | Flipped270 => flip_horizontal(image),
            _ => image.clone(),
        };
        match transform {
            Normal | Flipped => image,
            Rotate90 | Flipped90 => rotate270(&image),
            Rotate180 | Flipped180 => rotate180(&image),
            Rotate270 | Flipped270 => rotate90(&image),
        }
    }

    #[test]
    fn every_transform_is_undone_exactly() {
        use Transform::{Flipped, Flipped90, Flipped180, Flipped270, Normal, Rotate90, Rotate180, Rotate270};
        // 3x2, every pixel different: no symmetry can hide a wrong turn.
        let picture = RgbaImage::from_fn(3, 2, |x, y| {
            let byte = |v: u32| u8::try_from(v).expect("a 3x2 picture's coordinates fit a byte");
            image::Rgba([byte(x), byte(y), 0, 255])
        });
        for t in [
            Normal, Rotate90, Rotate180, Rotate270, Flipped, Flipped90, Flipped180, Flipped270,
        ] {
            let buffer = as_compositor_draws(&picture, t);
            assert_eq!(apply_transform(buffer, t), picture, "{t:?}");
        }
    }

    #[test]
    fn a_rotate90_buffer_is_turned_clockwise() {
        let img = apply_transform(red_green(), Transform::Rotate90);
        assert_eq!(img.dimensions(), (1, 2));
        // Clockwise: the left-hand pixel ends on top.
        assert_eq!(img.get_pixel(0, 0)[0], 255);
        assert_eq!(img.get_pixel(0, 1)[1], 255);
    }

    #[test]
    fn flipped_mirrors_horizontally() {
        let img = apply_transform(red_green(), Transform::Flipped);
        assert_eq!(img.get_pixel(0, 0)[1], 255);
    }

    #[test]
    fn rotate180_and_normal() {
        assert_eq!(
            apply_transform(red_green(), Transform::Rotate180).get_pixel(0, 0)[1],
            255
        );
        assert_eq!(apply_transform(red_green(), Transform::Normal), red_green());
    }

    #[test]
    fn preference_orders_formats() {
        assert!(PixelFormat::Xrgb8888.preference() < PixelFormat::Xbgr8888.preference());
        assert!(PixelFormat::Xbgr8888.preference() < PixelFormat::Xrgb2101010.preference());
    }

    #[test]
    fn banded_conversion_matches_row_by_row() {
        let (w, h, stride) = (7_u32, 301_u32, 7 * 4 + 8);
        let data: Vec<u8> = (0..stride * h).map(|i| (i * 31 % 251) as u8).collect();
        for y_invert in [false, true] {
            for format in [PixelFormat::Xrgb8888, PixelFormat::Abgr8888] {
                let one = to_bgra_banded(format, &data, (w, h, stride), y_invert, 1);
                for bands in [2, 8, 64, 1000] {
                    let many = to_bgra_banded(format, &data, (w, h, stride), y_invert, bands);
                    assert_eq!(many, one, "{format:?} y_invert={y_invert} bands={bands}");
                }
                assert_eq!(to_bgra(format, &data, w, h, stride, y_invert), one);
            }
        }
    }
}
