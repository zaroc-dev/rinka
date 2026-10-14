//! Video frames and YUV→RGB conversion.
//!
//! Hosts decode video themselves (FFmpeg natively, WebCodecs in the preview)
//! and hand frames to the engine as planar 8-bit Y'CbCr. The engine converts
//! them in the shader, so both hosts share one conversion with an explicit
//! matrix and range:
//!
//! - The frame's matrix (BT.601, BT.709, BT.2020 non-constant luminance) and
//!   range (limited/"TV" or full/"PC") map Y'CbCr to non-linear R'G'B'.
//! - Unspecified matrices follow the common player convention: BT.709 for
//!   frames at least 720 pixels tall, BT.601 below.
//! - R'G'B' is treated as sRGB-encoded, as browsers and desktop players
//!   display SDR video. The engine linearizes it like any authored color.
//! - Video is opaque. Audio is not played: wallpapers are muted.

/// Y'CbCr → R'G'B' coefficients.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorMatrix {
    Bt601,
    #[default]
    Bt709,
    Bt2020Ncl,
}

impl ColorMatrix {
    /// The fallback for streams that do not say.
    pub fn guess(height: u32) -> Self {
        if height >= 720 {
            Self::Bt709
        } else {
            Self::Bt601
        }
    }

    /// Luma coefficients (Kr, Kb).
    fn kr_kb(self) -> (f32, f32) {
        match self {
            Self::Bt601 => (0.299, 0.114),
            Self::Bt709 => (0.2126, 0.0722),
            Self::Bt2020Ncl => (0.2627, 0.0593),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorRange {
    /// Y in 16..=235, chroma in 16..=240 (8-bit).
    #[default]
    Limited,
    /// Full 0..=255.
    Full,
}

/// Rows of an affine matrix mapping normalized (Y, Cb, Cr, 1) samples, as
/// read from `R8Unorm` textures, to R'G'B'.
pub fn yuv_to_rgb(matrix: ColorMatrix, range: ColorRange) -> [[f32; 4]; 3] {
    let (kr, kb) = matrix.kr_kb();
    let kg = 1.0 - kr - kb;
    // Scale and offset from stored code values to Y in 0..1, C in -0.5..0.5.
    let (y_scale, y_offset, c_scale) = match range {
        ColorRange::Limited => (255.0 / 219.0, 16.0 / 255.0, 255.0 / 224.0),
        ColorRange::Full => (1.0, 0.0, 1.0),
    };
    let c_offset = 128.0 / 255.0;
    // R = Y + (2 - 2Kr) Cr
    // G = Y - (2Kb(1-Kb)/Kg) Cb - (2Kr(1-Kr)/Kg) Cr
    // B = Y + (2 - 2Kb) Cb
    let r_cr = 2.0 - 2.0 * kr;
    let b_cb = 2.0 - 2.0 * kb;
    let g_cb = -2.0 * kb * (1.0 - kb) / kg;
    let g_cr = -2.0 * kr * (1.0 - kr) / kg;
    let row = |cb: f32, cr: f32| {
        let (ys, cbs, crs) = (y_scale, cb * c_scale, cr * c_scale);
        [ys, cbs, crs, -y_offset * ys - c_offset * (cbs + crs)]
    };
    [row(0.0, r_cr), row(g_cb, g_cr), row(b_cb, 0.0)]
}

/// One planar 8-bit Y'CbCr frame (4:2:0, 4:2:2 or 4:4:4), borrowed from
/// the decoder.
pub struct YuvFrame<'a> {
    /// Luma size.
    pub width: u32,
    pub height: u32,
    /// Size of each chroma plane.
    pub chroma_width: u32,
    pub chroma_height: u32,
    pub y: &'a [u8],
    pub y_stride: u32,
    pub u: &'a [u8],
    pub u_stride: u32,
    pub v: &'a [u8],
    pub v_stride: u32,
    pub matrix: ColorMatrix,
    pub range: ColorRange,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: [[f32; 4]; 3], y: u8, u: u8, v: u8) -> [f32; 3] {
        let s = [
            f32::from(y) / 255.0,
            f32::from(u) / 255.0,
            f32::from(v) / 255.0,
            1.0,
        ];
        m.map(|row| row.iter().zip(s).map(|(a, b)| a * b).sum())
    }

    #[test]
    fn limited_range_black_and_white() {
        let m = yuv_to_rgb(ColorMatrix::Bt709, ColorRange::Limited);
        for (c, want) in apply(m, 16, 128, 128).into_iter().zip([0.0; 3]) {
            assert!((c - want).abs() < 1e-4, "{c}");
        }
        for c in apply(m, 235, 128, 128) {
            assert!((c - 1.0).abs() < 1e-4, "{c}");
        }
    }

    #[test]
    fn bt709_primaries() {
        // BT.709 limited-range red: Y=63, Cb=102, Cr=240.
        let rgb = apply(
            yuv_to_rgb(ColorMatrix::Bt709, ColorRange::Limited),
            63,
            102,
            240,
        );
        assert!(
            (rgb[0] - 1.0).abs() < 0.01 && rgb[1].abs() < 0.01 && rgb[2].abs() < 0.01,
            "{rgb:?}"
        );
        // BT.601 full-range blue: Y=29, Cb=255, Cr=107.
        let rgb = apply(
            yuv_to_rgb(ColorMatrix::Bt601, ColorRange::Full),
            29,
            255,
            107,
        );
        assert!(
            rgb[0].abs() < 0.01 && rgb[1].abs() < 0.01 && (rgb[2] - 1.0).abs() < 0.01,
            "{rgb:?}"
        );
    }
}
