//! Color conventions.
//!
//! - Authored colors are 8-bit sRGB-encoded, non-premultiplied.
//! - The engine composes in linear light with premultiplied alpha.
//! - Render targets are written through an sRGB-format view, so the hardware
//!   re-encodes linear values. An authored solid color therefore reaches the
//!   presented buffer with its original 8-bit values on every backend.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An opaque sRGB-encoded color, written as `#rrggbb` in project files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Srgb8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Srgb8 {
    pub const BLACK: Self = Self::new(0, 0, 0);

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Linear-light RGBA with alpha 1.0.
    pub fn to_linear(self) -> [f32; 4] {
        [
            srgb_to_linear(self.r),
            srgb_to_linear(self.g),
            srgb_to_linear(self.b),
            1.0,
        ]
    }
}

/// sRGB electro-optical transfer function (IEC 61966-2-1).
pub fn srgb_to_linear(v: u8) -> f32 {
    let c = f32::from(v) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid color {0:?}: expected #rrggbb")]
pub struct ParseColorError(String);

impl FromStr for Srgb8 {
    type Err = ParseColorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseColorError(s.to_owned());
        let hex = s.strip_prefix('#').ok_or_else(err)?;
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(err());
        }
        let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| err());
        Ok(Self::new(channel(0)?, channel(2)?, channel(4)?))
    }
}

impl fmt::Display for Srgb8 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl Serialize for Srgb8 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Srgb8 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats_hex() {
        let c: Srgb8 = "#1E6f73".parse().unwrap();
        assert_eq!(c, Srgb8::new(0x1e, 0x6f, 0x73));
        assert_eq!(c.to_string(), "#1e6f73");
        for bad in ["1e6f73", "#1e6f7", "#1e6f7g", "#1e6f7300", ""] {
            assert!(bad.parse::<Srgb8>().is_err(), "{bad}");
        }
    }

    #[test]
    fn transfer_function_endpoints() {
        assert_eq!(srgb_to_linear(0), 0.0);
        assert_eq!(srgb_to_linear(255), 1.0);
        assert!((srgb_to_linear(128) - 0.2158605).abs() < 1e-6);
    }
}
