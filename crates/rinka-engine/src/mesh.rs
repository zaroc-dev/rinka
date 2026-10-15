//! Layer meshes: a triangle mesh over an image layer's texture, optionally
//! skinned to a skeleton that animations move (Wallpaper Engine's puppet
//! warp imports as this).
//!
//! A mesh is an asset of its own (`rinka.mesh`, JSON) so large meshes stay
//! out of the project manifest. Positions are layer pixels relative to the
//! layer's center, x right, y down, before the layer's scale and rotation;
//! texture coordinates have (0, 0) at the texture's top-left.
//!
//! Without animations a skinned mesh shows its bind pose. With animation
//! layers, each bone's local transform is built from the layers' frames and
//! the bind pose, and every vertex follows its bones through
//! `world · bind⁻¹`. Poses are a pure function of the scene time, computed in
//! `f64` with `libm`, so native and WebAssembly builds agree and seeking is
//! exact.

use serde::{Deserialize, Serialize};

pub const FORMAT: &str = "rinka.mesh";
pub const VERSION: u32 = 1;

/// Floats per animation frame: position xyz, rotation xyz (radians, applied
/// x, then y, then z) and scale xyz.
pub const FRAME_FLOATS: usize = 9;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mesh {
    pub format: String,
    pub version: u32,
    /// `[x0, y0, x1, y1, …]`.
    pub positions: Vec<f32>,
    /// `[u0, v0, u1, v1, …]`.
    pub uvs: Vec<f32>,
    /// Triangles, three vertex indices each.
    pub indices: Vec<u32>,
    /// Four bone indices per vertex (empty for an unskinned mesh).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joints: Vec<u32>,
    /// Four bone weights per vertex, matching `joints`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weights: Vec<f32>,
    /// Parents come before their children.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bones: Vec<Bone>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub animations: Vec<Animation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bone {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,
    /// Bind pose relative to the parent: a column-major 4×4 matrix.
    pub matrix: [f32; 16],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Animation {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    pub fps: f32,
    /// Frames in one pass; tracks hold this many frames or one more (the
    /// closing frame).
    pub length: u32,
    #[serde(default)]
    pub mode: PlayMode,
    /// One track per bone: [`FRAME_FLOATS`] floats per frame, bone-local.
    pub tracks: Vec<Vec<f32>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayMode {
    #[default]
    Loop,
    /// Forward, then backward.
    Mirror,
    /// Once, then holds the last frame.
    Single,
}

/// An animation playing on a layer's mesh (part of the layer, not the
/// mesh asset).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshAnimation {
    /// [`Animation::id`].
    pub animation: i64,
    #[serde(default = "one")]
    pub rate: f32,
    /// How much this layer moves the bones, 0..1.
    #[serde(default = "one")]
    pub blend: f32,
    #[serde(default = "yes")]
    pub visible: bool,
}

fn one() -> f32 {
    1.0
}

fn yes() -> bool {
    true
}

#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    #[error("not a mesh: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unknown mesh format {0}")]
    Format(String),
    #[error("mesh version {0} is newer than this engine supports ({VERSION})")]
    Version(u32),
    #[error("mesh is inconsistent: {0}")]
    Invalid(&'static str),
}

impl Mesh {
    pub fn from_json(text: &str) -> Result<Self, MeshError> {
        let mesh: Mesh = serde_json::from_str(text)?;
        mesh.validate()?;
        Ok(mesh)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("meshes serialize")
    }

    pub fn vertex_count(&self) -> usize {
        self.positions.len() / 2
    }

    pub fn validate(&self) -> Result<(), MeshError> {
        use MeshError::Invalid;
        if self.format != FORMAT {
            return Err(MeshError::Format(self.format.clone()));
        }
        if self.version > VERSION {
            return Err(MeshError::Version(self.version));
        }
        let n = self.vertex_count();
        if !self.positions.len().is_multiple_of(2) || self.uvs.len() != 2 * n {
            return Err(Invalid("positions and texture coordinates disagree"));
        }
        if !self.indices.len().is_multiple_of(3) || self.indices.iter().any(|&i| i as usize >= n) {
            return Err(Invalid("triangle indices are out of range"));
        }
        let skinned = !self.joints.is_empty();
        if skinned && (self.joints.len() != 4 * n || self.weights.len() != 4 * n) {
            return Err(Invalid("joints and weights need four per vertex"));
        }
        if self.joints.iter().any(|&j| j as usize >= self.bones.len()) {
            return Err(Invalid("a vertex refers to a missing bone"));
        }
        for (i, bone) in self.bones.iter().enumerate() {
            if bone.parent.is_some_and(|p| p as usize >= i) {
                return Err(Invalid("a bone comes before its parent"));
            }
        }
        for anim in &self.animations {
            if anim.tracks.len() != self.bones.len() {
                return Err(Invalid("an animation needs one track per bone"));
            }
            let frames_ok = |t: &Vec<f32>| {
                let frames = t.len() / FRAME_FLOATS;
                t.len().is_multiple_of(FRAME_FLOATS) && frames >= anim.length.max(1) as usize
            };
            if !anim.tracks.iter().all(frames_ok) || anim.fps.is_nan() || anim.fps <= 0.0 {
                return Err(Invalid("an animation's tracks are shorter than its length"));
            }
        }
        Ok(())
    }
}

// ---- math --------------------------------------------------------------------

/// Column-major 4×4 matrix.
type M4 = [f64; 16];

const IDENTITY: M4 = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

fn mul(a: &M4, b: &M4) -> M4 {
    let mut out = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            out[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    out
}

/// Inverse of an affine matrix (last row 0 0 0 1); `None` if singular.
fn inverse_affine(m: &M4) -> Option<M4> {
    let a = |r: usize, c: usize| m[c * 4 + r];
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1))
        - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0))
        + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = |r: usize, c: usize| {
        // Cofactor of (c, r), transposed.
        let (r0, r1) = match c {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        let (c0, c1) = match r {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        let minor = a(r0, c0) * a(r1, c1) - a(r0, c1) * a(r1, c0);
        let sign = if (r + c).is_multiple_of(2) { 1.0 } else { -1.0 };
        sign * minor / det
    };
    let mut out = IDENTITY;
    for r in 0..3 {
        for c in 0..3 {
            out[c * 4 + r] = inv(r, c);
        }
    }
    for r in 0..3 {
        out[12 + r] = -(0..3).map(|k| out[k * 4 + r] * m[12 + k]).sum::<f64>();
    }
    Some(out)
}

#[derive(Clone, Copy)]
struct Quat {
    w: f64,
    x: f64,
    y: f64,
    z: f64,
}

impl Quat {
    const IDENTITY: Quat = Quat {
        w: 1.0,
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    fn mul(self, o: Quat) -> Quat {
        Quat {
            w: self.w * o.w - self.x * o.x - self.y * o.y - self.z * o.z,
            x: self.w * o.x + self.x * o.w + self.y * o.z - self.z * o.y,
            y: self.w * o.y - self.x * o.z + self.y * o.w + self.z * o.x,
            z: self.w * o.z + self.x * o.y - self.y * o.x + self.z * o.w,
        }
    }

    fn conjugate(self) -> Quat {
        Quat {
            w: self.w,
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }

    fn axis(angle: f64, axis: usize) -> Quat {
        let (s, c) = (libm::sin(angle / 2.0), libm::cos(angle / 2.0));
        let mut q = Quat {
            w: c,
            ..Quat::IDENTITY
        };
        match axis {
            0 => q.x = s,
            1 => q.y = s,
            _ => q.z = s,
        }
        q
    }

    /// Rotation about x, then y, then z.
    fn euler(r: [f64; 3]) -> Quat {
        Quat::axis(r[2], 2)
            .mul(Quat::axis(r[1], 1))
            .mul(Quat::axis(r[0], 0))
    }

    /// From `self` toward `to` by `t`, along the shorter arc.
    fn slerp(self, to: Quat, t: f64) -> Quat {
        let mut dot = self.w * to.w + self.x * to.x + self.y * to.y + self.z * to.z;
        let to = if dot < 0.0 {
            dot = -dot;
            Quat {
                w: -to.w,
                x: -to.x,
                y: -to.y,
                z: -to.z,
            }
        } else {
            to
        };
        let (a, b) = if dot > 0.9995 {
            (1.0 - t, t)
        } else {
            let theta = libm::acos(dot.min(1.0));
            let s = libm::sin(theta);
            (libm::sin((1.0 - t) * theta) / s, libm::sin(t * theta) / s)
        };
        let q = Quat {
            w: a * self.w + b * to.w,
            x: a * self.x + b * to.x,
            y: a * self.y + b * to.y,
            z: a * self.z + b * to.z,
        };
        let len = libm::sqrt(q.w * q.w + q.x * q.x + q.y * q.y + q.z * q.z);
        Quat {
            w: q.w / len,
            x: q.x / len,
            y: q.y / len,
            z: q.z / len,
        }
    }
}

/// translation · rotation · scale.
fn trs(t: [f64; 3], q: Quat, s: [f64; 3]) -> M4 {
    let Quat { w, x, y, z } = q;
    let r = [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y + w * z),
            2.0 * (x * z - w * y),
        ],
        [
            2.0 * (x * y - w * z),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z + w * x),
        ],
        [
            2.0 * (x * z + w * y),
            2.0 * (y * z - w * x),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ];
    let mut m = IDENTITY;
    for c in 0..3 {
        for row in 0..3 {
            m[c * 4 + row] = r[c][row] * s[c];
        }
    }
    m[12..15].copy_from_slice(&t);
    m
}

// ---- playback ------------------------------------------------------------------

struct Frame {
    position: [f64; 3],
    rotation: Quat,
    scale: [f64; 3],
}

fn frame(track: &[f32], i: usize) -> Frame {
    let f = &track[i * FRAME_FLOATS..(i + 1) * FRAME_FLOATS];
    let v = |k: usize| [f64::from(f[k]), f64::from(f[k + 1]), f64::from(f[k + 2])];
    Frame {
        position: v(0),
        rotation: Quat::euler(v(3)),
        scale: v(6),
    }
}

/// The two frames around `seconds` into an animation, and how far between.
fn frames_at(anim: &Animation, seconds: f64) -> (usize, usize, f64) {
    let length = f64::from(anim.length.max(1));
    let stored = anim.tracks.first().map_or(1, |t| t.len() / FRAME_FLOATS);
    // Tracks with a closing frame interpolate into it; others wrap to 0.
    let closing = stored > anim.length as usize;
    let at = (seconds * f64::from(anim.fps)).max(0.0);
    let pos = match anim.mode {
        PlayMode::Loop => at % length,
        PlayMode::Mirror => {
            let p = at % (2.0 * length);
            if p <= length { p } else { 2.0 * length - p }
        }
        PlayMode::Single => at.min(length),
    };
    let a = libm::floor(pos) as usize;
    let t = pos - a as f64;
    let last = if closing {
        anim.length as usize
    } else {
        anim.length as usize - 1
    };
    let a = a.min(last);
    let b = if a < last {
        a + 1
    } else if closing || anim.mode != PlayMode::Loop {
        last
    } else {
        0
    };
    (a, b, t)
}

/// One animation layer resolved against the mesh.
struct Active<'a> {
    anim: &'a Animation,
    rate: f64,
    /// The layer's own blend.
    blend: f64,
    /// Its share of the bones' base pose.
    share: f64,
}

/// Plays a mesh's animation layers.
pub struct MeshPlayer {
    /// Inverse bind-pose world matrices.
    inverse_bind: Vec<M4>,
    world: Vec<M4>,
    skin: Vec<M4>,
}

impl MeshPlayer {
    pub fn new(mesh: &Mesh) -> Self {
        let mut world: Vec<M4> = Vec::with_capacity(mesh.bones.len());
        for bone in &mesh.bones {
            let local = bone.matrix.map(f64::from);
            let w = match bone.parent {
                Some(p) => mul(&world[p as usize], &local),
                None => local,
            };
            world.push(w);
        }
        let inverse_bind = world
            .iter()
            .map(|w| inverse_affine(w).unwrap_or(IDENTITY))
            .collect();
        Self {
            inverse_bind,
            world: vec![IDENTITY; mesh.bones.len()],
            skin: vec![IDENTITY; mesh.bones.len()],
        }
    }

    /// Whether any layer moves the mesh.
    pub fn animated(mesh: &Mesh, layers: &[MeshAnimation]) -> bool {
        !mesh.joints.is_empty() && !Self::resolve(mesh, layers, &mut 0.0).is_empty()
    }

    /// Visible layers whose animation exists, with their shares of the
    /// base pose; `rest` gets what is left for the bind pose. The top layer
    /// takes its blend first; if the blends add up to more than 1, they are
    /// normalized instead.
    fn resolve<'a>(mesh: &'a Mesh, layers: &[MeshAnimation], rest: &mut f64) -> Vec<Active<'a>> {
        let mut active: Vec<Active> = layers
            .iter()
            .filter(|l| l.visible)
            .filter_map(|l| {
                let anim = mesh.animations.iter().find(|a| a.id == l.animation)?;
                Some(Active {
                    anim,
                    rate: f64::from(l.rate),
                    blend: f64::from(l.blend.clamp(0.0, 1.0)),
                    share: 0.0,
                })
            })
            .collect();
        let total: f64 = active.iter().map(|a| a.blend).sum();
        *rest = 1.0;
        for a in active.iter_mut().rev() {
            if total > 1.0 {
                a.share = a.blend / total;
            } else {
                a.share = *rest * a.blend;
                *rest *= 1.0 - a.blend;
            }
        }
        if total > 1.0 {
            *rest = 0.0;
        }
        active
    }

    /// Writes the mesh's vertex positions at scene time `time` into `out` as
    /// `[x, y]` pairs.
    pub fn positions(
        &mut self,
        mesh: &Mesh,
        layers: &[MeshAnimation],
        time: f64,
        out: &mut Vec<f32>,
    ) {
        out.clear();
        let mut rest = 1.0;
        let active = Self::resolve(mesh, layers, &mut rest);
        if mesh.joints.is_empty() || active.is_empty() {
            out.extend_from_slice(&mesh.positions);
            return;
        }
        for (i, bone) in mesh.bones.iter().enumerate() {
            let bind_t = [12, 13, 14].map(|k| f64::from(bone.matrix[k]));
            let mut t = bind_t.map(|v| v * rest);
            let mut s = [rest; 3];
            let mut q = Quat::IDENTITY;
            for a in &active {
                let track = &a.anim.tracks[i];
                let (fa, fb, k) = frames_at(a.anim, time * a.rate);
                let (base, fa, fb) = (frame(track, 0), frame(track, fa), frame(track, fb));
                for c in 0..3 {
                    let lerp = |x: [f64; 3], y: [f64; 3]| x[c] + (y[c] - x[c]) * k;
                    t[c] += a.share * base.position[c]
                        + a.blend * (lerp(fa.position, fb.position) - base.position[c]);
                    s[c] += a.share * base.scale[c]
                        + a.blend * (lerp(fa.scale, fb.scale) - base.scale[c]);
                }
                // The change since the first frame at the layer's blend, the
                // first frame itself at the layer's share.
                let inv_base = base.rotation.conjugate();
                let delta = fa
                    .rotation
                    .mul(inv_base)
                    .slerp(fb.rotation.mul(inv_base), k);
                q = q
                    .mul(Quat::IDENTITY.slerp(delta, a.blend))
                    .mul(Quat::IDENTITY.slerp(base.rotation, a.share));
            }
            let q = q.slerp(Quat::IDENTITY, rest);
            let local = trs(t, q, s);
            self.world[i] = match bone.parent {
                Some(p) => mul(&self.world[p as usize], &local),
                None => local,
            };
            self.skin[i] = mul(&self.world[i], &self.inverse_bind[i]);
        }
        for v in 0..mesh.vertex_count() {
            let (x, y) = (
                f64::from(mesh.positions[2 * v]),
                f64::from(mesh.positions[2 * v + 1]),
            );
            let (mut ox, mut oy, mut total) = (0.0, 0.0, 0.0);
            for j in 0..4 {
                let w = f64::from(mesh.weights[4 * v + j]);
                if w == 0.0 {
                    continue;
                }
                let m = &self.skin[mesh.joints[4 * v + j] as usize];
                ox += w * (m[0] * x + m[4] * y + m[12]);
                oy += w * (m[1] * x + m[5] * y + m[13]);
                total += w;
            }
            if total > 0.0 {
                out.extend_from_slice(&[(ox / total) as f32, (oy / total) as f32]);
            } else {
                out.extend_from_slice(&[x as f32, y as f32]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translate(x: f32, y: f32) -> [f32; 16] {
        let mut m = [0.0; 16];
        for i in 0..4 {
            m[i * 5] = 1.0;
        }
        m[12] = x;
        m[13] = y;
        m
    }

    /// Two vertices on two bones; the child bone's animation moves it right
    /// by 10 px over one second.
    fn mesh() -> Mesh {
        let still = |x: f32, y: f32| vec![x, y, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        Mesh {
            format: FORMAT.into(),
            version: VERSION,
            positions: vec![0.0, 0.0, 5.0, 0.0],
            uvs: vec![0.0; 4],
            indices: vec![0, 1, 1],
            joints: vec![0, 0, 0, 0, 1, 0, 0, 0],
            weights: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
            bones: vec![
                Bone {
                    parent: None,
                    matrix: translate(0.0, 0.0),
                },
                Bone {
                    parent: Some(0),
                    matrix: translate(5.0, 0.0),
                },
            ],
            animations: vec![Animation {
                id: 7,
                name: String::new(),
                fps: 1.0,
                length: 1,
                mode: PlayMode::Loop,
                tracks: vec![
                    [still(0.0, 0.0), still(0.0, 0.0)].concat(),
                    [still(5.0, 0.0), still(15.0, 0.0)].concat(),
                ],
            }],
        }
    }

    fn layer(blend: f32) -> MeshAnimation {
        MeshAnimation {
            animation: 7,
            rate: 1.0,
            blend,
            visible: true,
        }
    }

    #[test]
    fn bind_pose_without_animation() {
        let m = mesh();
        let mut out = Vec::new();
        MeshPlayer::new(&m).positions(&m, &[], 0.5, &mut out);
        assert_eq!(out, m.positions);
    }

    #[test]
    fn animation_moves_and_blends() {
        let m = mesh();
        m.validate().unwrap();
        let mut p = MeshPlayer::new(&m);
        let mut out = Vec::new();
        p.positions(&m, &[layer(1.0)], 0.5, &mut out);
        assert!((out[2] - 10.0).abs() < 1e-4, "{out:?}");
        p.positions(&m, &[layer(0.5)], 0.5, &mut out);
        assert!((out[2] - 7.5).abs() < 1e-4, "{out:?}");
        // Seeking is a function of time alone.
        let mut again = Vec::new();
        MeshPlayer::new(&m).positions(&m, &[layer(0.5)], 0.5, &mut again);
        assert_eq!(out, again);
    }

    #[test]
    fn inverse_undoes_rotation() {
        let m = trs(
            [3.0, -2.0, 1.0],
            Quat::euler([0.3, -0.2, 1.1]),
            [2.0, 0.5, 1.0],
        );
        let id = mul(&m, &inverse_affine(&m).unwrap());
        for (a, b) in id.iter().zip(IDENTITY) {
            assert!((a - b).abs() < 1e-9);
        }
    }

    #[test]
    fn rejects_bad_indices() {
        let mut m = mesh();
        m.indices = vec![0, 1, 2];
        assert!(m.validate().is_err());
    }
}
