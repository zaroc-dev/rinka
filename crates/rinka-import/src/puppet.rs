//! Wallpaper Engine puppet warp models (`.mdl`), converted to Rinka meshes.
//!
//! Layout, little-endian, as observed in `MDLV0017`/`MDLV0023`, `MDLS0002`/
//! `MDLS0004` and `MDLA0004`/`MDLA0006` files:
//!
//! - `MDLVnnnn\0` · i32 flags · i32 · i32 · material path `\0` · a few words
//!   (zero, or a small count in `MDLV0021`+) · optionally a format word (`0x01800009`: 52-byte vertices, `0x0180000F`:
//!   80-byte vertices with a normal and tangent after the position) · u32
//!   vertex bytes · vertices (position xyz, [normal, tangent], 4 × u32 bone,
//!   4 × f32 weight, uv) · u32 index bytes · u16 triangles. Newer versions
//!   put more data here (`MDLV0023`: the vertices' unwarped positions); it
//!   is skipped up to the `MDLS` tag.
//! - `MDLSnnnn\0` · u32 end offset · u32 bones · per bone: name `\0` · u32 ·
//!   u32 parent (`0xFFFFFFFF` for none) · u32 64 · 4×4 column-major local
//!   bind matrix · JSON constraints `\0`.
//! - `MDATnnnn\0` (optional, seen as `MDAT0001` before `MDLA`) · u32 end
//!   offset · u16 attachments · per attachment: u16 bone · name `\0` ·
//!   4×4 column-major matrix relative to the bone.
//! - `MDLAnnnn\0` · u32 end offset · u32 animations · per animation: u32 id ·
//!   u32 · name `\0` (an empty one is followed by the name) · mode `\0`
//!   (`loop`, `mirror`, `single`) · f32 fps · u32 length · u32 · u32 tracks ·
//!   per track: u32 · u32 bytes · frames of 9 × f32 (position, rotation,
//!   scale). Gaps between animations differ by version; each header is found
//!   by validating it. `MDLA0006` stores more tracks than bones; the extra
//!   ones are not bones and are skipped.
//!
//! Puppet data is y up; Rinka meshes are y down. Converting mirrors y:
//! positions and translations negate y, matrices are conjugated by
//! diag(1, −1, 1) and rotations about x and z change sign.

use rinka_engine::mesh::{self, Animation, Attachment, Bone, Mesh, PlayMode};

#[derive(Debug, thiserror::Error)]
pub enum MdlError {
    #[error("not a puppet model ({0})")]
    Magic(String),
    #[error("puppet model is truncated or corrupt")]
    Truncated,
    #[error("unsupported puppet vertex data ({0} bytes)")]
    Vertices(u32),
    #[error("puppet bone matrix of {0} bytes")]
    Matrix(u32),
    #[error("puppet attachment {0} refers to a missing bone")]
    Attachment(String),
    #[error("puppet bone {0} comes before its parent")]
    Parent(usize),
    #[error("puppet animation {0} could not be read")]
    Animation(usize),
}

/// Constraint settings of a bone that the engine does not simulate.
#[derive(Clone, Debug, Default)]
pub struct Constraints {
    pub ik: bool,
    pub physics: bool,
}

pub struct Puppet {
    pub mesh: Mesh,
    /// One per bone.
    pub constraints: Vec<Constraints>,
    pub bone_names: Vec<String>,
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn u32(&mut self) -> Result<u32, MdlError> {
        let b = self
            .data
            .get(self.pos..self.pos + 4)
            .ok_or(MdlError::Truncated)?;
        self.pos += 4;
        Ok(u32::from_le_bytes(b.try_into().expect("4 bytes")))
    }

    fn f32(&mut self) -> Result<f32, MdlError> {
        self.u32().map(f32::from_bits)
    }

    fn u16(&mut self) -> Result<u16, MdlError> {
        let b = self
            .data
            .get(self.pos..self.pos + 2)
            .ok_or(MdlError::Truncated)?;
        self.pos += 2;
        Ok(u16::from_le_bytes(b.try_into().expect("2 bytes")))
    }

    fn string(&mut self) -> Result<String, MdlError> {
        let rest = self.data.get(self.pos..).ok_or(MdlError::Truncated)?;
        let end = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or(MdlError::Truncated)?;
        let s = String::from_utf8_lossy(&rest[..end]).into_owned();
        self.pos += end + 1;
        Ok(s)
    }

    /// Moves just past the next `TAGnnnn\0` at or after the cursor and
    /// returns its version.
    fn seek_tag(&mut self, tag: &[u8; 4]) -> Result<u32, MdlError> {
        let rest = self.data.get(self.pos..).ok_or(MdlError::Truncated)?;
        let at = rest
            .windows(9)
            .position(|w| &w[..4] == tag && w[4..8].iter().all(u8::is_ascii_digit) && w[8] == 0)
            .ok_or(MdlError::Truncated)?;
        let version = std::str::from_utf8(&rest[at + 4..at + 8])
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        self.pos += at + 9;
        Ok(version)
    }
}

const FORMAT_SHORT: u32 = 0x0180_0009;
const FORMAT_LONG: u32 = 0x0180_000F;

pub fn parse(data: &[u8]) -> Result<Puppet, MdlError> {
    let mut r = Reader { data, pos: 0 };
    let magic = data.get(..8).ok_or(MdlError::Truncated)?;
    if &magic[..4] != b"MDLV" {
        return Err(MdlError::Magic(String::from_utf8_lossy(magic).into_owned()));
    }
    r.pos = 9;
    let _flags = r.u32()?;
    let _ = (r.u32()?, r.u32()?);
    let _material = r.string()?;

    // A few words (mostly zero), a format word, then the vertex bytes; files
    // without a format word have the vertex bytes as the first nonzero word.
    let start = r.pos;
    let format = (0..64).find_map(|i| {
        let mut t = Reader {
            data,
            pos: start + 4 * i,
        };
        match t.u32().ok()? {
            FORMAT_SHORT => Some((52, t)),
            FORMAT_LONG => Some((80, t)),
            _ => None,
        }
    });
    let (stride, vertex_bytes) = match format {
        Some((stride, mut t)) => {
            let bytes = t.u32()?;
            r.pos = t.pos;
            (stride, bytes)
        }
        None => {
            let mut word = r.u32()?;
            while word == 0 {
                word = r.u32()?;
            }
            (52, word)
        }
    };
    if vertex_bytes == 0 || vertex_bytes % stride != 0 {
        return Err(MdlError::Vertices(vertex_bytes));
    }
    let n = (vertex_bytes / stride) as usize;
    let mut positions = Vec::with_capacity(2 * n);
    let mut uvs = Vec::with_capacity(2 * n);
    let mut joints = Vec::with_capacity(4 * n);
    let mut weights = Vec::with_capacity(4 * n);
    for _ in 0..n {
        let (x, y, _z) = (r.f32()?, r.f32()?, r.f32()?);
        if stride == 80 {
            r.pos += 7 * 4; // normal, tangent
        }
        for _ in 0..4 {
            joints.push(r.u32()?);
        }
        for _ in 0..4 {
            weights.push(r.f32()?);
        }
        let (u, v) = (r.f32()?, r.f32()?);
        positions.extend_from_slice(&[x, -y]);
        uvs.extend_from_slice(&[u, v]);
    }
    let index_bytes = r.u32()?;
    let mut indices = Vec::with_capacity(index_bytes as usize / 2);
    for _ in 0..index_bytes / 2 {
        indices.push(u32::from(r.u16()?));
    }
    // Triangles keep their winding: mirroring y flips it, and nothing culls.

    let mut bones = Vec::new();
    let mut constraints = Vec::new();
    let mut bone_names = Vec::new();
    if r.seek_tag(b"MDLS").is_ok() {
        let _end = r.u32()?;
        let count = r.u32()? as usize;
        for i in 0..count.min(4096) {
            bone_names.push(r.string()?);
            let _ = r.u32()?;
            let parent = match r.u32()? {
                u32::MAX => None,
                p if (p as usize) < i => Some(p),
                _ => return Err(MdlError::Parent(i)),
            };
            let size = r.u32()?;
            if size != 64 {
                return Err(MdlError::Matrix(size));
            }
            let mut m = [0f32; 16];
            for v in &mut m {
                *v = r.f32()?;
            }
            bones.push(Bone {
                parent,
                matrix: mirror_matrix(m),
            });
            let json = r.string()?;
            let flag =
                |v: &serde_json::Value, k: &str| v.get(k).and_then(|b| b.as_bool()) == Some(true);
            let c = serde_json::from_str::<serde_json::Value>(&json)
                .map(|v| Constraints {
                    ik: flag(&v, "ik"),
                    physics: flag(&v, "r") || flag(&v, "t"),
                })
                .unwrap_or_default();
            constraints.push(c);
        }
    }

    let mut attachments = Vec::new();
    let mut a = Reader { data, pos: r.pos };
    if !bones.is_empty() && a.seek_tag(b"MDAT").is_ok() {
        let _end = a.u32()?;
        let count = a.u16()?;
        for _ in 0..count {
            let bone = u32::from(a.u16()?);
            let name = a.string()?;
            if bone as usize >= bones.len() {
                return Err(MdlError::Attachment(name));
            }
            let mut m = [0f32; 16];
            for v in &mut m {
                *v = a.f32()?;
            }
            attachments.push(Attachment {
                name,
                bone,
                matrix: mirror_matrix(m),
            });
        }
    }

    let mut animations = Vec::new();
    if !bones.is_empty() && r.seek_tag(b"MDLA").is_ok() {
        let _end = r.u32()?;
        let count = r.u32()? as usize;
        for i in 0..count.min(4096) {
            let at = |skip: usize| {
                let mut t = Reader {
                    data,
                    pos: r.pos + skip,
                };
                let (anim, plain) = read_animation(&mut t, bones.len()).ok()?;
                Some((anim, plain, t.pos))
            };
            let (first, found) = (0..4096)
                .find_map(|skip| Some((skip, at(skip)?)))
                .ok_or(MdlError::Animation(i))?;
            // Zero bytes before the id read as a shifted id too, and a start
            // a byte late can still parse. Of the offsets that read to the
            // same end, the header is the earliest whose word after the id
            // is zero and whose name follows directly (as in every observed
            // file), else the earliest that reads at all.
            let end = found.2;
            let candidates: Vec<_> = std::iter::once(found)
                .chain((first + 1..first + 4).filter_map(at))
                .filter(|c| c.2 == end)
                .collect();
            let best = candidates.iter().position(|c| c.1).unwrap_or(0);
            let anim = candidates.into_iter().nth(best).expect("one candidate").0;
            r.pos = end;
            animations.push(anim);
        }
    }

    let skinned = !bones.is_empty();
    let mesh = Mesh {
        format: mesh::FORMAT.into(),
        version: mesh::VERSION,
        positions,
        uvs,
        indices,
        joints: if skinned { joints } else { Vec::new() },
        weights: if skinned { weights } else { Vec::new() },
        bones,
        animations,
        attachments,
    };
    Ok(Puppet {
        mesh,
        constraints,
        bone_names,
    })
}

/// Reads one animation at the cursor, or fails if the bytes there are not
/// a plausible animation header.
///
/// Also returns whether the header had the observed plain shape: a zero
/// word after the id and the name right after it.
fn read_animation(r: &mut Reader, bones: usize) -> Result<(Animation, bool), MdlError> {
    let bad = || MdlError::Truncated;
    let id = r.u32()?;
    if id == 0 || id > 0x7fff_ffff {
        return Err(bad());
    }
    let word = r.u32()?;
    let mut name = r.string()?;
    let plain = word == 0 && !name.is_empty();
    if name.is_empty() {
        name = r.string()?;
    }
    if name.len() > 256 || name.chars().any(char::is_control) {
        return Err(bad());
    }
    let mode = match r.string()?.as_str() {
        "loop" | "" => PlayMode::Loop,
        "mirror" => PlayMode::Mirror,
        "single" => PlayMode::Single,
        _ => return Err(bad()),
    };
    let fps = r.f32()?;
    let length = r.u32()?;
    let _ = r.u32()?;
    let tracks = r.u32()? as usize;
    if !(fps > 0.0 && fps < 1000.0) || length == 0 || length > 1_000_000 || tracks < bones {
        return Err(bad());
    }
    let mut out = Vec::with_capacity(bones);
    let mut frame_bytes = None;
    let mut read_track = |r: &mut Reader| -> Result<Vec<f32>, MdlError> {
        let _ = r.u32()?;
        let bytes = r.u32()?;
        let floats = bytes as usize / 4;
        if bytes % (mesh::FRAME_FLOATS as u32 * 4) != 0
            || floats / mesh::FRAME_FLOATS < length as usize
            || frame_bytes.is_some_and(|b| b != bytes)
        {
            return Err(bad());
        }
        frame_bytes = Some(bytes);
        let mut track = Vec::with_capacity(floats);
        for f in 0..floats {
            let v = r.f32()?;
            // position x y z · rotation x y z · scale x y z, mirrored in y.
            track.push(match f % mesh::FRAME_FLOATS {
                1 | 3 | 5 => -v,
                _ => v,
            });
        }
        Ok(track)
    };
    for i in 0..tracks {
        let track = read_track(r)?;
        if i < bones {
            out.push(track);
        }
    }
    // Tracks beyond the declared count with the same size (`MDLA0006`).
    loop {
        let mark = r.pos;
        if read_track(r).is_err() {
            r.pos = mark;
            break;
        }
    }
    let anim = Animation {
        id: i64::from(id),
        name,
        fps,
        length,
        mode,
        tracks: out,
    };
    Ok((anim, plain))
}

/// Conjugates a column-major matrix by diag(1, −1, 1, 1).
fn mirror_matrix(mut m: [f32; 16]) -> [f32; 16] {
    for c in 0..4 {
        for r in 0..4 {
            if (r == 1) != (c == 1) {
                m[c * 4 + r] = -m[c * 4 + r];
            }
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirroring_flips_y_translation_and_z_rotation() {
        // Rotation by +90° about z (counterclockwise, y up) with a translation.
        let m = [
            0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 3.0, 4.0, 0.0, 1.0,
        ];
        let f = mirror_matrix(m);
        assert_eq!(&f[12..15], &[3.0, -4.0, 0.0]);
        // Now clockwise in y-down terms: x axis maps to (0, -1).
        assert_eq!(&f[0..2], &[0.0, -1.0]);
    }
}
