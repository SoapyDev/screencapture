//! Conversions between the number types that screens, pixels and drawings use.
//!
//! Each conversion states its limit once, here, instead of an `as` cast at every use: desktop
//! coordinates, pixel counts and sizes stay far inside every range involved (a screen is a few
//! thousand pixels; `f32` is exact up to 2^24, about 16 million).

/// A number as `f32`, for gpui and drawing.
pub trait ToF32: Copy {
    /// The nearest `f32`: exact for anything a screen or an image counts.
    fn to_f32(self) -> f32;
}

/// `as f32` for the integer types, whose values here stay below 2^24.
macro_rules! exact_below_2_pow_24 {
    ($($integer:ty),*) => {$(
        impl ToF32 for $integer {
            #[expect(
                clippy::cast_precision_loss,
                reason = "coordinates, sizes and counts stay below 2^24, where f32 is exact"
            )]
            fn to_f32(self) -> f32 {
                self as f32
            }
        }
    )*};
}

exact_below_2_pow_24!(i32, u32, usize, u64);

impl ToF32 for f64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "scales and positions need no more than f32's precision"
    )]
    fn to_f32(self) -> f32 {
        self as f32
    }
}

/// `value` as `i32`, cut toward zero, saturating at `i32`'s ends (NaN gives 0).
#[expect(
    clippy::cast_possible_truncation,
    reason = "Rust's float-to-int cast saturates: that is the wanted limit"
)]
pub fn saturating_i32(value: impl Into<f64>) -> i32 {
    value.into() as i32
}

/// `value` as `u32`, cut toward zero, negatives giving 0 (and NaN 0).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "Rust's float-to-int cast saturates: that is the wanted limit"
)]
pub fn saturating_u32(value: impl Into<f64>) -> u32 {
    value.into() as u32
}

/// `value` as `u8`, cut toward zero, saturating at 0 and 255 (NaN gives 0).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "Rust's float-to-int cast saturates: that is the wanted limit"
)]
pub fn saturating_u8(value: impl Into<f64>) -> u8 {
    value.into() as u8
}

/// `value` as `usize`, cut toward zero, negatives giving 0 (and NaN 0).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "Rust's float-to-int cast saturates: that is the wanted limit"
)]
pub fn saturating_usize(value: impl Into<f64>) -> usize {
    value.into() as usize
}

/// `value` as `u32`, negatives giving 0.
#[must_use]
pub const fn unsigned(value: i32) -> u32 {
    if value < 0 { 0 } else { value.cast_unsigned() }
}

/// `value` as `i32`, saturating at `i32::MAX`.
#[must_use]
pub const fn signed(value: u32) -> i32 {
    if value > i32::MAX.cast_unsigned() {
        i32::MAX
    } else {
        value.cast_signed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_of_any_screen_are_exact_as_f32() {
        assert_eq!((-16_777_216_i32).to_f32(), -16_777_216.0);
    }

    #[test]
    fn a_scale_keeps_its_value_as_f32() {
        assert_eq!(1.5_f64.to_f32(), 1.5);
    }

    #[test]
    fn a_float_is_cut_toward_zero() {
        assert_eq!(saturating_i32(-2.7_f32), -2);
    }

    #[test]
    fn a_float_beyond_i32_saturates() {
        assert_eq!(saturating_i32(1e12_f64), i32::MAX);
    }

    #[test]
    fn not_a_number_gives_zero() {
        assert_eq!(saturating_u32(f32::NAN), 0);
    }

    #[test]
    fn a_negative_float_gives_no_u32() {
        assert_eq!(saturating_u32(-3.0_f32), 0);
    }

    #[test]
    fn a_colour_channel_saturates_at_255() {
        assert_eq!(saturating_u8(300.0_f32), 255);
    }

    #[test]
    fn a_float_count_becomes_a_usize() {
        assert_eq!(saturating_usize(31.9_f32), 31);
    }

    #[test]
    fn a_negative_size_gives_no_unsigned() {
        assert_eq!(unsigned(-5), 0);
    }

    #[test]
    fn a_size_keeps_its_value_unsigned() {
        assert_eq!(unsigned(2560), 2560);
    }

    #[test]
    fn a_huge_count_saturates_signed() {
        assert_eq!(signed(u32::MAX), i32::MAX);
    }

    #[test]
    fn a_count_keeps_its_value_signed() {
        assert_eq!(signed(1440), 1440);
    }
}
