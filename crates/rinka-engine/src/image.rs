//! Still-image decoding.
//!
//! Every host decodes through this module, so native playback and the WASM
//! preview see identical pixels. Decoding is CPU work and belongs outside the
//! render path.
//!
//! Conventions:
//! - PNG and JPEG only. Embedded ICC profiles are ignored; pixel values are
//!   treated as sRGB.
//! - EXIF orientation is applied.
//! - Images larger than the texture limit are downscaled to fit. Placement
//!   always uses the original size (`source_size`), so a downscaled texture
//!   only loses sharpness, never position: hosts may choose a lower limit
//!   (the editor preview does) without changing what the scene means.
//! - Fully opaque images stay 8-bit sRGB. Images with any transparency are
//!   converted to premultiplied linear half floats, so filtering and mipmaps
//!   do not bleed the color of invisible pixels.

use std::io::Cursor;

use image::{DynamicImage, ImageDecoder, ImageReader, imageops::FilterType};

use crate::color::srgb_to_linear;
use crate::gpu;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// `Rgba8UnormSrgb`, straight alpha (always 255).
    Srgba8,
    /// `Rgba8Unorm`, straight alpha, values exactly as stored. Used by
    /// gamma-space scenes, whose textures are not color-decoded.
    Rgba8Raw,
    /// `Rgba16Float`, linear light, premultiplied alpha.
    LinearPremulF16,
}

impl PixelFormat {
    pub fn texture_format(self) -> wgpu::TextureFormat {
        match self {
            Self::Srgba8 => wgpu::TextureFormat::Rgba8UnormSrgb,
            Self::Rgba8Raw => wgpu::TextureFormat::Rgba8Unorm,
            Self::LinearPremulF16 => wgpu::TextureFormat::Rgba16Float,
        }
    }

    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Srgba8 | Self::Rgba8Raw => 4,
            Self::LinearPremulF16 => 8,
        }
    }
}

/// Decoded pixels ready for upload.
#[derive(Clone)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub pixels: Vec<u8>,
    /// Size after orientation, before any downscale. Placement uses this.
    pub source_size: (u32, u32),
}

impl std::fmt::Debug for ImageData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageData")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format)
            .field("source_size", &self.source_size)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("unrecognized image format (supported: PNG, JPEG)")]
    Format,
    #[error("could not decode image: {0}")]
    Decode(#[from] image::ImageError),
}

/// Decodes without any color conversion: straight alpha, stored values.
/// Scenes composed in gamma space use this.
pub fn decode_raw(bytes: &[u8], max_dimension: u32) -> Result<ImageData, ImageError> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| ImageError::Format)?;
    if reader.format().is_none() {
        return Err(ImageError::Format);
    }
    let mut img = reader.decode()?;
    let source_size = (img.width(), img.height());
    let max = max_dimension.clamp(1, gpu::required_limits().max_texture_dimension_2d);
    if img.width() > max || img.height() > max {
        img = img.resize(max, max, FilterType::Triangle);
    }
    let rgba = img.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(ImageData {
        width,
        height,
        format: PixelFormat::Rgba8Raw,
        pixels: rgba.into_raw(),
        source_size,
    })
}

/// Decodes at up to the profile's texture limit.
pub fn decode(bytes: &[u8]) -> Result<ImageData, ImageError> {
    decode_with_limit(bytes, gpu::required_limits().max_texture_dimension_2d)
}

/// Decodes, downscaling so neither side exceeds `max_dimension` (clamped to
/// the profile's texture limit).
pub fn decode_with_limit(bytes: &[u8], max_dimension: u32) -> Result<ImageData, ImageError> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| ImageError::Format)?;
    if reader.format().is_none() {
        return Err(ImageError::Format);
    }
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    let source_size = (img.width(), img.height());

    let max = max_dimension.clamp(1, gpu::required_limits().max_texture_dimension_2d);
    if img.width() > max || img.height() > max {
        img = img.resize(max, max, FilterType::Triangle);
    }

    let rgba = img.into_rgba8();
    let (width, height) = rgba.dimensions();
    let opaque = rgba.pixels().all(|p| p.0[3] == 255);
    let (format, pixels) = if opaque {
        (PixelFormat::Srgba8, rgba.into_raw())
    } else {
        let lut: Vec<f32> = (0..=255u8).map(srgb_to_linear).collect();
        let mut out = Vec::with_capacity(rgba.len() * 2);
        for p in rgba.pixels() {
            let a = f32::from(p.0[3]) / 255.0;
            for c in [
                lut[usize::from(p.0[0])] * a,
                lut[usize::from(p.0[1])] * a,
                lut[usize::from(p.0[2])] * a,
                a,
            ] {
                out.extend_from_slice(&half::f16::from_f32(c).to_le_bytes());
            }
        }
        (PixelFormat::LinearPremulF16, out)
    };
    Ok(ImageData {
        width,
        height,
        format,
        pixels,
        source_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba, RgbaImage};

    fn png(img: &RgbaImage) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn opaque_png_stays_srgb8() {
        let img = RgbaImage::from_fn(3, 2, |x, y| Rgba([x as u8 * 100, y as u8 * 200, 7, 255]));
        let data = decode(&png(&img)).unwrap();
        assert_eq!(
            (data.width, data.height, data.format),
            (3, 2, PixelFormat::Srgba8)
        );
        assert_eq!(data.pixels, img.into_raw());
    }

    #[test]
    fn transparent_png_becomes_premultiplied_linear() {
        let img = RgbaImage::from_pixel(1, 1, Rgba([255, 0, 0, 128]));
        let data = decode(&png(&img)).unwrap();
        assert_eq!(data.format, PixelFormat::LinearPremulF16);
        let px: Vec<f32> = data
            .pixels
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| half::f16::from_le_bytes(*b).to_f32())
            .collect();
        let a = 128.0 / 255.0;
        assert!(
            (px[0] - a).abs() < 1e-3 && px[1] == 0.0 && (px[3] - a).abs() < 1e-3,
            "{px:?}"
        );
    }

    #[test]
    fn downscaling_keeps_the_source_size() {
        let img = RgbaImage::from_pixel(40, 10, Rgba([1, 2, 3, 255]));
        let data = decode_with_limit(&png(&img), 8).unwrap();
        assert_eq!((data.width, data.height), (8, 2));
        assert_eq!(data.source_size, (40, 10));
    }

    #[test]
    fn rejects_unknown_formats() {
        assert!(matches!(
            decode(b"definitely not an image"),
            Err(ImageError::Format)
        ));
    }
}
