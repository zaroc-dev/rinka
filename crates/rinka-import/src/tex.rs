//! Wallpaper Engine `.tex` textures (`TEXV0005`).
//!
//! ```text
//! "TEXV0005\0" "TEXI0001\0"
//! i32 format, i32 flags, i32 texture w/h, i32 image w/h,
//! [flags & VOLUME: i32 depth] u32 unknown
//! "TEXB000N\0" i32 image count [N≥3: i32 FreeImage format] [N=4: i32 is-MP4]
//! per image: i32 mip count, per mip:
//!     i32 w, i32 h, [VOLUME: i32 depth],
//!     [N≥2: i32 LZ4 flag, i32 decompressed size], i32 size, bytes
//! [flags & GIF: "TEXS000N\0" i32 frame count [N=3: i32 w, h] frames]
//! ```
//!
//! Pixel data is allocated at the (power-of-two) texture size; the image
//! occupies its top-left `image w × h`. When a FreeImage format is given, the
//! mip bytes are an encoded file (PNG, JPEG, …) instead of raw pixels.
//!
//! Decoded pixels keep OpenGL's channel meaning so shaders see what Wallpaper
//! Engine shows them: R8 becomes (r, 0, 0, 255) and RG88 (r, g, 0, 255).
//! Values are not color-converted.

use image::RgbaImage;

pub const FLAG_NO_INTERPOLATION: u32 = 1;
pub const FLAG_CLAMP_UVS: u32 = 2;
pub const FLAG_ANIMATED: u32 = 4;
/// Volume texture (color lookup tables): the header carries a depth.
pub const FLAG_VOLUME: u32 = 0x40;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum TexFormat {
    Rgba8888,
    Dxt5,
    Dxt3,
    Dxt1,
    Rg88,
    R8,
}

impl TexFormat {
    fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            0 => Self::Rgba8888,
            4 => Self::Dxt5,
            6 => Self::Dxt3,
            7 => Self::Dxt1,
            8 => Self::Rg88,
            9 => Self::R8,
            _ => return None,
        })
    }
}

/// One frame of an animated (sprite-sheet) texture.
#[derive(Clone, Debug, serde::Serialize)]
pub struct TexFrame {
    pub image: usize,
    /// Seconds this frame shows.
    pub duration: f32,
    /// Rectangle in the image, in pixels.
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

pub struct Texture {
    pub format: TexFormat,
    pub flags: u32,
    /// Size of the meaningful image region.
    pub width: u32,
    pub height: u32,
    /// Allocated texture size; the image is its top-left region.
    pub texture_width: u32,
    pub texture_height: u32,
    /// Mip 0 of every image. Static textures are cropped to the image region;
    /// animated textures keep the full sheet their frames index into.
    pub images: Vec<RgbaImage>,
    pub frames: Vec<TexFrame>,
    /// The texture is an MP4 video, not pixels (`TEXB0004`).
    pub video: Option<Vec<u8>>,
}

impl Texture {
    pub fn animated(&self) -> bool {
        self.flags & FLAG_ANIMATED != 0 && !self.frames.is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TexError {
    #[error("not a texture ({0})")]
    Magic(String),
    #[error("texture is truncated or corrupt")]
    Truncated,
    #[error("unsupported texture format {0}")]
    Format(i32),
    #[error("LZ4 data is corrupt: {0}")]
    Lz4(#[from] lz4_flex::block::DecompressError),
    #[error("embedded image: {0}")]
    Image(#[from] image::ImageError),
    #[error("pixel data does not match {0}×{1}")]
    Size(u32, u32),
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], TexError> {
        let b = self
            .data
            .get(self.pos..self.pos + n)
            .ok_or(TexError::Truncated)?;
        self.pos += n;
        Ok(b)
    }

    fn i32(&mut self) -> Result<i32, TexError> {
        Ok(i32::from_le_bytes(
            self.bytes(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn f32(&mut self) -> Result<f32, TexError> {
        Ok(f32::from_le_bytes(
            self.bytes(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn tag(&mut self) -> Result<String, TexError> {
        let rest = self.data.get(self.pos..).ok_or(TexError::Truncated)?;
        let end = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or(TexError::Truncated)?;
        let tag = String::from_utf8_lossy(&rest[..end]).into_owned();
        self.pos += end + 1;
        Ok(tag)
    }
}

pub fn decode(data: &[u8]) -> Result<Texture, TexError> {
    let mut r = Reader { data, pos: 0 };
    let magic = r.tag()?;
    if magic != "TEXV0005" {
        return Err(TexError::Magic(magic));
    }
    let info = r.tag()?;
    if info != "TEXI0001" {
        return Err(TexError::Magic(info));
    }
    let format_code = r.i32()?;
    let flags = r.i32()? as u32;
    let (texture_width, texture_height) = (r.i32()? as u32, r.i32()? as u32);
    let (width, height) = (r.i32()? as u32, r.i32()? as u32);
    if flags & FLAG_VOLUME != 0 {
        // 32³ lookup tables are stored as a 1024×32 strip of slices.
        let _depth = r.i32()?;
    }
    let _unknown = r.i32()?;

    let container = r.tag()?;
    let version: u32 = container
        .strip_prefix("TEXB")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| TexError::Magic(container.clone()))?;
    let image_count = r.i32()?;
    let free_image = if version >= 3 { r.i32()? } else { -1 };
    let is_mp4 = version >= 4 && r.i32()? != 0;
    let format = TexFormat::from_code(format_code);

    let mut images = Vec::new();
    let mut video = None;
    for _ in 0..image_count.max(0) {
        let mips = r.i32()?;
        for mip in 0..mips.max(0) {
            let (mw, mh) = (r.i32()? as u32, r.i32()? as u32);
            if flags & FLAG_VOLUME != 0 {
                let _depth = r.i32()?;
            }
            let (lz4, decompressed) = if version >= 2 {
                (r.i32()? != 0, r.i32()? as usize)
            } else {
                (false, 0)
            };
            let size = r.i32()? as usize;
            let raw = r.bytes(size)?;
            if mip > 0 {
                continue; // The engine generates its own mipmaps.
            }
            let bytes = if lz4 {
                lz4_flex::block::decompress(raw, decompressed)?
            } else {
                raw.to_vec()
            };
            if is_mp4 {
                video = Some(bytes);
                continue;
            }
            let rgba = if free_image != -1 {
                image::load_from_memory(&bytes)?.into_rgba8()
            } else {
                let format = format.ok_or(TexError::Format(format_code))?;
                raw_to_rgba(format, mw, mh, &bytes)?
            };
            images.push(rgba);
        }
    }

    let mut frames = Vec::new();
    if flags & FLAG_ANIMATED != 0 && r.pos < data.len() {
        let tag = r.tag()?;
        let v: u32 = tag
            .strip_prefix("TEXS")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let count = r.i32()?;
        if v >= 3 {
            let _gif_w = r.i32()?;
            let _gif_h = r.i32()?;
        }
        for _ in 0..count.max(0) {
            let image = r.i32()? as usize;
            let duration = r.f32()?;
            let read = |r: &mut Reader| -> Result<f32, TexError> {
                if v == 1 { Ok(r.i32()? as f32) } else { r.f32() }
            };
            let x = read(&mut r)?;
            let y = read(&mut r)?;
            let width = read(&mut r)?;
            let _width_y = read(&mut r)?;
            let _height_x = read(&mut r)?;
            let height = read(&mut r)?;
            frames.push(TexFrame {
                image,
                duration,
                x,
                y,
                width,
                height,
            });
        }
    }

    // Static textures: keep only the image region.
    if frames.is_empty() {
        for img in &mut images {
            if img.width() > width || img.height() > height {
                *img = image::imageops::crop_imm(
                    img,
                    0,
                    0,
                    width.min(img.width()),
                    height.min(img.height()),
                )
                .to_image();
            }
        }
    }

    Ok(Texture {
        format: format.unwrap_or(TexFormat::Rgba8888),
        flags,
        width,
        height,
        texture_width,
        texture_height,
        images,
        frames,
        video,
    })
}

fn raw_to_rgba(format: TexFormat, w: u32, h: u32, data: &[u8]) -> Result<RgbaImage, TexError> {
    let px = (w * h) as usize;
    let pixels = match format {
        TexFormat::Rgba8888 => data.get(..px * 4).ok_or(TexError::Size(w, h))?.to_vec(),
        TexFormat::Rg88 => {
            let src = data.get(..px * 2).ok_or(TexError::Size(w, h))?;
            src.as_chunks::<2>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], 0, 255])
                .collect()
        }
        TexFormat::R8 => {
            let src = data.get(..px).ok_or(TexError::Size(w, h))?;
            src.iter().flat_map(|&v| [v, 0, 0, 255]).collect()
        }
        TexFormat::Dxt1 | TexFormat::Dxt3 | TexFormat::Dxt5 => decode_bc(format, w, h, data)?,
    };
    RgbaImage::from_raw(w, h, pixels).ok_or(TexError::Size(w, h))
}

/// Decodes BC1 (DXT1), BC2 (DXT3) and BC3 (DXT5) blocks.
fn decode_bc(format: TexFormat, w: u32, h: u32, data: &[u8]) -> Result<Vec<u8>, TexError> {
    let block_bytes = if format == TexFormat::Dxt1 { 8 } else { 16 };
    let (bw, bh) = (w.div_ceil(4) as usize, h.div_ceil(4) as usize);
    if data.len() < bw * bh * block_bytes {
        return Err(TexError::Size(w, h));
    }
    let mut out = vec![0u8; (w * h * 4) as usize];
    for by in 0..bh {
        for bx in 0..bw {
            let block = &data[(by * bw + bx) * block_bytes..][..block_bytes];
            let (alpha, color) = match format {
                TexFormat::Dxt1 => (None, block),
                _ => (Some(&block[..8]), &block[8..]),
            };
            let colors = bc1_palette(color, format == TexFormat::Dxt1);
            let indices = u32::from_le_bytes(color[4..8].try_into().expect("4 bytes"));
            let alphas = match (format, alpha) {
                (TexFormat::Dxt5, Some(a)) => Some(bc3_alpha(a)),
                _ => None,
            };
            for i in 0..16 {
                let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
                if x >= w as usize || y >= h as usize {
                    continue;
                }
                let mut c = colors[((indices >> (2 * i)) & 3) as usize];
                match (format, alpha) {
                    (TexFormat::Dxt3, Some(a)) => {
                        let nibble = (a[i / 2] >> (4 * (i % 2))) & 0xf;
                        c[3] = nibble * 17;
                    }
                    (TexFormat::Dxt5, _) => c[3] = alphas.expect("alpha")[i],
                    _ => {}
                }
                out[(y * w as usize + x) * 4..][..4].copy_from_slice(&c);
            }
        }
    }
    Ok(out)
}

fn rgb565(v: u16) -> [u8; 3] {
    let r = (v >> 11) & 31;
    let g = (v >> 5) & 63;
    let b = v & 31;
    [
        ((r << 3) | (r >> 2)) as u8,
        ((g << 2) | (g >> 4)) as u8,
        ((b << 3) | (b >> 2)) as u8,
    ]
}

fn bc1_palette(block: &[u8], allow_transparent: bool) -> [[u8; 4]; 4] {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let (a, b) = (rgb565(c0), rgb565(c1));
    let mix = |wa: u16, wb: u16, d: u16| {
        let m = |i: usize| ((u16::from(a[i]) * wa + u16::from(b[i]) * wb) / d) as u8;
        [m(0), m(1), m(2), 255]
    };
    if c0 > c1 || !allow_transparent {
        [
            [a[0], a[1], a[2], 255],
            [b[0], b[1], b[2], 255],
            mix(2, 1, 3),
            mix(1, 2, 3),
        ]
    } else {
        [
            [a[0], a[1], a[2], 255],
            [b[0], b[1], b[2], 255],
            mix(1, 1, 2),
            [0, 0, 0, 0],
        ]
    }
}

fn bc3_alpha(block: &[u8]) -> [u8; 16] {
    let (a0, a1) = (u16::from(block[0]), u16::from(block[1]));
    let mut table = [0u8; 8];
    table[0] = a0 as u8;
    table[1] = a1 as u8;
    if a0 > a1 {
        for i in 1..7 {
            table[i + 1] = (((7 - i as u16) * a0 + i as u16 * a1) / 7) as u8;
        }
    } else {
        for i in 1..5 {
            table[i + 1] = (((5 - i as u16) * a0 + i as u16 * a1) / 5) as u8;
        }
        table[6] = 0;
        table[7] = 255;
    }
    let bits = block[2..8]
        .iter()
        .rev()
        .fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
    std::array::from_fn(|i| table[((bits >> (3 * i)) & 7) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bc1_opaque_block() {
        // c0 = pure red (0xF800), c1 = pure blue (0x001F), all pixels index 0
        // except the last, which uses index 1.
        let mut block = vec![0x00, 0xF8, 0x1F, 0x00];
        block.extend((1u32 << 30).to_le_bytes());
        let out = decode_bc(TexFormat::Dxt1, 4, 4, &block).unwrap();
        assert_eq!(&out[..4], &[255, 0, 0, 255]);
        assert_eq!(&out[60..64], &[0, 0, 255, 255]);
    }

    #[test]
    fn bc3_alpha_ramp() {
        let a = bc3_alpha(&[255, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(a, [255; 16]);
        // Index 1 everywhere selects a1.
        let ones: u64 = (0..16).fold(0, |acc, i| acc | (1 << (3 * i)));
        let mut block = vec![200, 10];
        block.extend(&ones.to_le_bytes()[..6]);
        assert_eq!(bc3_alpha(&block), [10; 16]);
    }

    #[test]
    fn single_channel_formats_keep_gl_meaning() {
        let r8 = raw_to_rgba(TexFormat::R8, 2, 1, &[7, 9]).unwrap();
        assert_eq!(r8.as_raw(), &[7, 0, 0, 255, 9, 0, 0, 255]);
        let rg = raw_to_rgba(TexFormat::Rg88, 1, 1, &[3, 4]).unwrap();
        assert_eq!(rg.as_raw(), &[3, 4, 0, 255]);
    }
}
