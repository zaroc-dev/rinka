//! Wallpaper Engine particle definitions → Rinka particle systems.
//!
//! Field meanings follow Wallpaper Engine's documentation of the particle
//! editor. Defaults for fields a definition leaves out are not documented;
//! they are the values two open implementations (linux-wallpaperengine and
//! wallpaper-scene-renderer) agree on, and are written out explicitly so
//! the engine has no hidden defaults. Both also feed half the random size to
//! Wallpaper Engine's particle shader, which draws that as the sprite width.
//!
//! Wallpaper Engine's particle space has y up; Rinka's has y down, so every
//! vector's y is mirrored (and ranges swap ends).

use rinka_engine::particles::{
    Blend, Emitter, EmitterShape, Initializer, Operator, Particles, SheetMode, SpriteRenderer,
    SpriteSheet,
};
use serde_json::Value;

use crate::report::Item;
use crate::value;

/// Multipliers from a particle object's `instanceoverride`.
#[derive(Clone, Copy, Debug)]
pub struct Overrides {
    pub alpha: f32,
    pub size: f32,
    pub lifetime: f32,
    pub speed: f32,
    pub rate: f32,
    pub count: f32,
}

impl Default for Overrides {
    fn default() -> Self {
        Self {
            alpha: 1.0,
            size: 1.0,
            lifetime: 1.0,
            speed: 1.0,
            rate: 1.0,
            count: 1.0,
        }
    }
}

impl Overrides {
    pub fn read(o: &Value) -> Self {
        let f = |k: &str| o.get(k).and_then(value::float).unwrap_or(1.0);
        Self {
            alpha: f("alpha"),
            size: f("size"),
            lifetime: f("lifetime"),
            speed: f("speed"),
            rate: f("rate"),
            count: f("count"),
        }
    }
}

/// A child system and where it sits relative to its parent (y up).
pub struct Child {
    pub path: String,
    pub origin: [f32; 3],
    pub scale: [f32; 3],
    pub angles: [f32; 3],
}

pub struct Converted {
    pub system: Particles,
    /// Systems drawn with this one at its origin.
    pub children: Vec<Child>,
}

fn float(c: &Value, key: &str, default: f32) -> f32 {
    c.get(key).and_then(value::float).unwrap_or(default)
}

/// A vector field; a single number fills every component.
fn vec3(c: &Value, key: &str, default: [f32; 3]) -> [f32; 3] {
    match c.get(key).and_then(value::floats) {
        Some(f) if f.len() >= 3 => [f[0], f[1], f[2]],
        Some(f) if f.len() == 1 => [f[0]; 3],
        _ => default,
    }
}

/// Mirrors a y range: `min..max` becomes `-max..-min`.
fn mirror_range(min: [f32; 3], max: [f32; 3]) -> ([f32; 2], [f32; 2]) {
    ([min[0], -max[1]], [max[0], -min[1]])
}

fn blend(c: &Value) -> Option<Blend> {
    let keys = ["blendinstart", "blendinend", "blendoutstart", "blendoutend"];
    if keys.iter().all(|k| c.get(*k).is_none()) {
        return None;
    }
    Some([
        float(c, keys[0], 0.0),
        float(c, keys[1], 0.0),
        float(c, keys[2], 1.0),
        float(c, keys[3], 1.0),
    ])
}

fn name(c: &Value) -> &str {
    c.get("name").and_then(Value::as_str).unwrap_or("?")
}

/// Converts one particle definition. Returns `None` when nothing could be
/// drawn (no supported emitter or renderer); `item` records what was lost.
pub fn convert(def: &Value, o: &Overrides, seed: u64, item: &mut Item) -> Option<Converted> {
    let list = |k: &str| {
        def.get(k)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };

    let flags = def.get("flags").and_then(Value::as_u64).unwrap_or(0);
    if flags & 4 != 0 {
        item.partial("perspective particles are drawn flat");
    }

    // Renderer first: rope systems cannot be drawn at all.
    let renderer = match list("renderer").first() {
        None => SpriteRenderer::Sprite,
        Some(r) => match name(r) {
            "sprite" => {
                let orientation = r.get("orientation").and_then(value::float).unwrap_or(0.0);
                if orientation != 0.0 {
                    item.partial("sprite orientation other than facing the screen is drawn facing the screen");
                }
                SpriteRenderer::Sprite
            }
            "spritetrail" => SpriteRenderer::Trail {
                length: float(r, "length", 0.05),
                min_length: float(r, "minlength", 0.0),
                max_length: float(r, "maxlength", 10.0),
            },
            other => {
                item.unsupported(format!("the {other} renderer is not supported"));
                return None;
            }
        },
    };

    let mut emitters = Vec::new();
    for e in list("emitter") {
        let dmin = vec3(&e, "distancemin", [0.0; 3]);
        let dmax = vec3(&e, "distancemax", [256.0; 3]);
        let shape = match name(&e) {
            "sphererandom" => EmitterShape::Disk {
                radius: [dmin[0], dmax[0]],
            },
            "boxrandom" => EmitterShape::Box {
                min: [dmin[0], dmin[1]],
                max: [dmax[0], dmax[1]],
            },
            other => {
                item.partial(format!("emitter {other} is not supported"));
                continue;
            }
        };
        let directions = vec3(&e, "directions", [1.0, 1.0, 0.0]);
        let origin = vec3(&e, "origin", [0.0; 3]);
        let sign = vec3(&e, "sign", [0.0; 3]);
        let flags = e.get("flags").and_then(Value::as_u64).unwrap_or(0);
        if flags & 4 != 0 {
            item.partial("periodic emission is not supported; the emitter runs continuously");
        }
        if float(&e, "audioprocessingmode", 0.0) != 0.0 {
            item.partial("audio-responsive emission is not supported");
        }
        let cp = float(&e, "controlpoint", 0.0);
        if cp != 0.0 {
            item.partial(format!(
                "the emitter follows control point {cp}; it stays at the system origin"
            ));
        }
        emitters.push(Emitter {
            shape,
            offset: [origin[0], -origin[1]],
            scale: [directions[0], directions[1]],
            sign: [
                sign[0].signum() as i8 * i8::from(sign[0] != 0.0),
                -(sign[1].signum() as i8) * i8::from(sign[1] != 0.0),
            ],
            rate: float(&e, "rate", 5.0) * o.rate * o.count,
            burst: (float(&e, "instantaneous", 0.0) * o.count).round().max(0.0) as u32,
            delay: float(&e, "delay", 0.0),
            duration: float(&e, "duration", 0.0),
            speed: [
                float(&e, "speedmin", 0.0) * o.speed,
                float(&e, "speedmax", 0.0) * o.speed,
            ],
            one_per_frame: flags & 2 != 0,
        });
    }
    if emitters.is_empty() {
        item.unsupported("the system has no supported emitter");
        return None;
    }

    let mut initializers = Vec::new();
    let mut has_alpha = false;
    for c in list("initializer") {
        let exponent = float(&c, "exponent", 1.0);
        let init = match name(&c) {
            "lifetimerandom" => Initializer::Lifetime {
                min: float(&c, "min", 0.0) * o.lifetime,
                max: float(&c, "max", 1.0) * o.lifetime,
                exponent,
            },
            "sizerandom" => Initializer::Size {
                min: float(&c, "min", 0.0) / 2.0 * o.size,
                max: float(&c, "max", 20.0) / 2.0 * o.size,
                exponent,
            },
            "alpharandom" => {
                has_alpha = true;
                Initializer::Alpha {
                    min: float(&c, "min", 0.05) * o.alpha,
                    max: float(&c, "max", 1.0) * o.alpha,
                    exponent,
                }
            }
            "colorrandom" => {
                let min = vec3(&c, "min", [0.0; 3]);
                let max = vec3(&c, "max", [255.0; 3]);
                Initializer::Color {
                    min: min.map(|v| v / 255.0),
                    max: max.map(|v| v / 255.0),
                    exponent,
                }
            }
            "velocityrandom" => {
                let s = o.speed;
                let (min, max) = mirror_range(
                    vec3(&c, "min", [-32.0, -32.0, 0.0]).map(|v| v * s),
                    vec3(&c, "max", [32.0, 32.0, 0.0]).map(|v| v * s),
                );
                Initializer::Velocity { min, max, exponent }
            }
            "rotationrandom" => Initializer::Rotation {
                min: vec3(&c, "min", [0.0; 3]),
                max: vec3(&c, "max", [0.0, 0.0, std::f32::consts::TAU]),
                exponent,
            },
            "angularvelocityrandom" => Initializer::AngularVelocity {
                min: vec3(&c, "min", [0.0, 0.0, -5.0]).map(|v| v * o.speed),
                max: vec3(&c, "max", [0.0, 0.0, 5.0]).map(|v| v * o.speed),
                exponent,
            },
            "turbulentvelocityrandom" => {
                let forward = vec3(&c, "forward", [0.0, 1.0, 0.0]);
                Initializer::TurbulentVelocity {
                    speed: [
                        float(&c, "speedmin", 100.0) * o.speed,
                        float(&c, "speedmax", 250.0) * o.speed,
                    ],
                    scale: float(&c, "scale", 1.0),
                    // Counterclockwise with y up is counterclockwise on
                    // screen; Rinka's angles turn clockwise.
                    offset: -float(&c, "offset", 0.0),
                    forward: [forward[0], -forward[1]],
                    noise_speed: float(&c, "timescale", 1.0),
                    phase: [float(&c, "phasemin", 0.0), float(&c, "phasemax", 0.1)],
                }
            }
            other => {
                item.partial(format!("initializer {other} is not supported"));
                continue;
            }
        };
        initializers.push(init);
    }
    if !has_alpha && o.alpha != 1.0 {
        initializers.push(Initializer::Alpha {
            min: o.alpha,
            max: o.alpha,
            exponent: 1.0,
        });
    }

    let mut operators = Vec::new();
    for c in list("operator") {
        let op = match name(&c) {
            "movement" => {
                let g = vec3(&c, "gravity", [0.0; 3]);
                Operator::Movement {
                    gravity: [g[0] * o.speed, -g[1] * o.speed],
                    drag: float(&c, "drag", 0.0),
                }
            }
            "angularmovement" => Operator::AngularMovement {
                force: vec3(&c, "force", [0.0; 3]),
                drag: float(&c, "drag", 0.0),
            },
            "alphafade" => Operator::AlphaFade {
                fade_in: float(&c, "fadeintime", 0.5),
                fade_out: float(&c, "fadeouttime", 0.5),
            },
            "sizechange" => Operator::SizeChange {
                time: [float(&c, "starttime", 0.0), float(&c, "endtime", 1.0)],
                value: [float(&c, "startvalue", 1.0), float(&c, "endvalue", 0.0)],
            },
            "alphachange" => Operator::AlphaChange {
                time: [float(&c, "starttime", 0.0), float(&c, "endtime", 1.0)],
                value: [float(&c, "startvalue", 1.0), float(&c, "endvalue", 0.0)],
            },
            "colorchange" => Operator::ColorChange {
                time: [float(&c, "starttime", 0.0), float(&c, "endtime", 1.0)],
                start: vec3(&c, "startvalue", [1.0; 3]),
                end: vec3(&c, "endvalue", [1.0; 3]),
            },
            n @ ("oscillatealpha" | "oscillatesize" | "oscillateposition") => {
                let fmin = float(&c, "frequencymin", 0.0);
                let fmax = float(
                    &c,
                    "frequencymax",
                    if n == "oscillateposition" { 5.0 } else { 10.0 },
                );
                let frequency = [fmin, if fmax == 0.0 { fmin } else { fmax }];
                let (smin, smax) = if n == "oscillatesize" {
                    (0.8, 1.2)
                } else {
                    (0.0, 1.0)
                };
                let scale = [float(&c, "scalemin", smin), float(&c, "scalemax", smax)];
                let phase = [
                    float(&c, "phasemin", 0.0),
                    float(&c, "phasemax", std::f32::consts::TAU),
                ];
                match n {
                    "oscillatealpha" => Operator::OscillateAlpha {
                        frequency,
                        scale,
                        phase,
                    },
                    "oscillatesize" => Operator::OscillateSize {
                        frequency,
                        scale,
                        phase,
                    },
                    _ => {
                        let mask = vec3(&c, "mask", [1.0, 1.0, 0.0]);
                        Operator::OscillatePosition {
                            frequency,
                            scale: scale.map(|v| v * o.speed),
                            phase,
                            mask: [mask[0], mask[1]],
                            blend: blend(&c),
                        }
                    }
                }
            }
            "turbulence" => {
                // Noise frequency per pixel; the field drifts `timescale`
                // pixels per second, and a particle's phase offsets it by
                // that many pixels.
                let k = float(&c, "scale", 0.01) * 2.0;
                let mask = vec3(&c, "mask", [1.0, 1.0, 0.0]);
                Operator::Turbulence {
                    speed: [
                        float(&c, "speedmin", 500.0) * o.speed,
                        float(&c, "speedmax", 1000.0) * o.speed,
                    ],
                    noise_scale: k,
                    noise_speed: float(&c, "timescale", 20.0) * k,
                    phase: [
                        float(&c, "phasemin", 0.0) * k,
                        float(&c, "phasemax", 0.0) * k,
                    ],
                    mask: [mask[0], mask[1]],
                    blend: blend(&c),
                }
            }
            other => {
                item.partial(format!("operator {other} is not supported"));
                continue;
            }
        };
        operators.push(op);
    }

    let mut children = Vec::new();
    for c in list("children") {
        let path = c
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        match c.get("type").and_then(Value::as_str).unwrap_or("static") {
            "static" => children.push(Child {
                path,
                origin: vec3(&c, "origin", [0.0; 3]),
                scale: vec3(&c, "scale", [1.0; 3]),
                angles: vec3(&c, "angles", [0.0; 3]),
            }),
            kind => item.partial(format!(
                "child system {path} ({kind}, one per particle) is not supported"
            )),
        }
    }

    let max_count = (float(def, "maxcount", 1.0) * o.count).round().max(1.0) as u32;
    Some(Converted {
        system: Particles {
            max_count,
            start_time: float(def, "starttime", 0.0).max(0.0),
            seed,
            emitters,
            initializers,
            operators,
            renderer,
            sheet: None,
        },
        children,
    })
}

/// How a system plays its texture's sprite-sheet `frames`.
pub fn sheet(def: &Value, frames: Vec<[f32; 4]>) -> SpriteSheet {
    let flags = def.get("flags").and_then(Value::as_u64).unwrap_or(0);
    let mode = match def.get("animationmode").and_then(Value::as_str) {
        Some("randomframe") => SheetMode::Random,
        Some("once") => SheetMode::Once,
        _ => SheetMode::Sequence,
    };
    SpriteSheet {
        frames,
        mode,
        speed: float(def, "sequencemultiplier", 1.0),
        // Flag 2: no blending between frames.
        blend: flags & 2 == 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Status;
    use serde_json::json;

    #[test]
    fn mirrors_y_and_applies_overrides() {
        let def = json!({
            "maxcount": 40,
            "starttime": 3,
            "emitter": [{"name": "sphererandom", "distancemax": 32, "origin": "0 768 0", "rate": 400}],
            "initializer": [
                {"name": "sizerandom", "min": 20, "max": 50},
                {"name": "velocityrandom", "min": "0 -3000 0", "max": "0 -2000 0"}
            ],
            "operator": [{"name": "movement", "gravity": "0 -100 0"}],
            "renderer": [{"name": "spritetrail", "length": 0.007}]
        });
        let o = Overrides {
            speed: 2.0,
            count: 0.5,
            ..Default::default()
        };
        let mut item = Item::new("t", "t", "particle system");
        let c = convert(&def, &o, 1, &mut item).unwrap();
        assert_eq!(item.status, Status::Supported);
        let s = c.system;
        assert_eq!(s.max_count, 20);
        assert_eq!(s.emitters[0].offset, [0.0, -768.0], "above the origin");
        assert_eq!(s.emitters[0].rate, 200.0);
        assert_eq!(
            s.initializers[0],
            Initializer::Size {
                min: 10.0,
                max: 25.0,
                exponent: 1.0
            }
        );
        // Falling (y down, positive), range ends swapped, times speed.
        assert_eq!(
            s.initializers[1],
            Initializer::Velocity {
                min: [0.0, 4000.0],
                max: [0.0, 6000.0],
                exponent: 1.0
            }
        );
        assert_eq!(
            s.operators[0],
            Operator::Movement {
                gravity: [0.0, 200.0],
                drag: 0.0
            }
        );
    }

    #[test]
    fn reports_what_it_cannot_do() {
        let def = json!({
            "flags": 4,
            "emitter": [{"name": "sphererandom", "flags": 4}],
            "operator": [{"name": "vortex"}],
            "children": [{"name": "particles/x.json", "type": "eventfollow"}]
        });
        let mut item = Item::new("t", "t", "particle system");
        let c = convert(&def, &Overrides::default(), 1, &mut item).unwrap();
        assert_eq!(item.status, Status::Partial);
        assert!(c.system.operators.is_empty() && c.children.is_empty());
        assert_eq!(item.notes.len(), 4, "{:?}", item.notes);

        let rope = json!({"emitter": [{"name": "sphererandom"}], "renderer": [{"name": "rope"}]});
        let mut item = Item::new("t", "t", "particle system");
        assert!(convert(&rope, &Overrides::default(), 1, &mut item).is_none());
        assert_eq!(item.status, Status::Unsupported);
    }
}
