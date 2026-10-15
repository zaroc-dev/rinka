//! Particle systems: the project format and a deterministic simulation.
//!
//! A particle layer is one system. Its particles live in the layer's local
//! space: canvas pixels, x right, y down, with the layer's origin, scale and
//! angle placing the whole system on the canvas. Times are seconds, colors
//! are encoded RGB in 0..1 like other scene colors.
//!
//! The simulation runs in fixed steps from the system's seed, so playing to a
//! scene time gives the same particles natively and in the preview, however
//! the frames fall. A jump of more than [`MAX_CATCH_UP`] restarts the system
//! that long before the target instead of simulating hours. Particles
//! keep a key from which operators draw their per-particle random values, so
//! nothing depends on iteration order. Transcendental functions come from
//! `libm`, so results do not depend on the platform's math library.

use serde::{Deserialize, Serialize};

/// Simulation step.
pub const STEP: f64 = 1.0 / 60.0;

/// The longest stretch of time simulated to reach a requested time. Farther
/// jumps (a scene rebuilt after hours) restart the system this long before
/// the requested time instead.
const MAX_CATCH_UP: f64 = 30.0;

/// Spacing of emissions with [`Emitter::one_per_frame`].
const FRAME: f64 = 1.0 / 30.0;

// ---- format ----------------------------------------------------------------

/// A particle system.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Particles {
    /// Most particles alive at once.
    pub max_count: u32,
    /// Seconds simulated before the scene starts, so it does not start empty.
    #[serde(default)]
    pub start_time: f32,
    /// Seed of the system's random numbers.
    #[serde(default)]
    pub seed: u64,
    pub emitters: Vec<Emitter>,
    #[serde(default)]
    pub initializers: Vec<Initializer>,
    /// Applied in order every step.
    #[serde(default)]
    pub operators: Vec<Operator>,
    #[serde(default)]
    pub renderer: SpriteRenderer,
    /// Animates the sprite through frames of the texture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sheet: Option<SpriteSheet>,
}

/// Frames of a sprite sheet and how particles step through them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpriteSheet {
    /// Frame rectangles in texture coordinates: u, v, width, height.
    pub frames: Vec<[f32; 4]>,
    #[serde(default)]
    pub mode: SheetMode,
    /// Times the sequence plays per particle life.
    #[serde(default = "one")]
    pub speed: f32,
    /// Cross-fades between neighbouring frames.
    #[serde(default = "yes")]
    pub blend: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SheetMode {
    /// Plays the frames over the particle's life, `speed` times, repeating.
    #[default]
    Sequence,
    /// Plays once and holds the last frame.
    Once,
    /// One random frame per particle.
    Random,
}

/// Where and how often particles appear.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Emitter {
    pub shape: EmitterShape,
    /// Center of the shape, relative to the system origin.
    #[serde(default)]
    pub offset: [f32; 2],
    /// Multiplies spawn positions per axis (stretches or flattens the shape).
    #[serde(default = "unit2")]
    pub scale: [f32; 2],
    /// Forces the sign of spawn positions per axis: 0 either, 1 positive,
    /// -1 negative.
    #[serde(default)]
    pub sign: [i8; 2],
    /// Particles per second.
    pub rate: f32,
    /// Particles created at once when the emitter starts.
    #[serde(default)]
    pub burst: u32,
    /// Seconds before the emitter starts.
    #[serde(default)]
    pub delay: f32,
    /// Seconds the emitter runs after its delay; 0 runs forever.
    #[serde(default)]
    pub duration: f32,
    /// Initial speed away from the center, min and max.
    #[serde(default)]
    pub speed: [f32; 2],
    /// At most one particle per 1/30 s.
    #[serde(default)]
    pub one_per_frame: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EmitterShape {
    /// Uniformly over the ring between two radii.
    Disk { radius: [f32; 2] },
    /// Per axis, a distance between `min` and `max` on either side.
    Box { min: [f32; 2], max: [f32; 2] },
}

/// Random starting values. `exponent` biases each draw: 1 is uniform,
/// below 1 favors `max`, above 1 favors `min`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Initializer {
    /// Seconds.
    Lifetime {
        min: f32,
        max: f32,
        #[serde(default = "one")]
        exponent: f32,
    },
    /// Width of the sprite in pixels; its height follows the texture.
    Size {
        min: f32,
        max: f32,
        #[serde(default = "one")]
        exponent: f32,
    },
    Alpha {
        min: f32,
        max: f32,
        #[serde(default = "one")]
        exponent: f32,
    },
    /// One draw mixes between the two colors.
    Color {
        min: [f32; 3],
        max: [f32; 3],
        #[serde(default = "one")]
        exponent: f32,
    },
    /// Pixels per second, drawn per axis; added to the emitter's speed.
    Velocity {
        min: [f32; 2],
        max: [f32; 2],
        #[serde(default = "one")]
        exponent: f32,
    },
    /// Radians about x, y and z; see [`sprite_axes`].
    Rotation {
        min: [f32; 3],
        max: [f32; 3],
        #[serde(default = "one")]
        exponent: f32,
    },
    /// Radians per second about x, y and z.
    AngularVelocity {
        min: [f32; 3],
        max: [f32; 3],
        #[serde(default = "one")]
        exponent: f32,
    },
    /// A velocity whose direction wanders smoothly over time around
    /// `forward`, like gusts in a stream of smoke.
    TurbulentVelocity {
        /// Pixels per second, min and max.
        speed: [f32; 2],
        /// How far the direction strays: 0 keeps to `forward`, 1 reaches
        /// every direction, 2 lets the stream wind over itself.
        scale: f32,
        /// Turns the direction, radians clockwise.
        offset: f32,
        /// Main direction.
        forward: [f32; 2],
        /// How fast the direction wanders.
        noise_speed: f32,
        /// Per-particle offset into the noise, min and max.
        phase: [f32; 2],
    },
}

/// A window within a particle's life (fractions 0..1) in which an operator
/// works: it fades in between the first two values and out between the
/// last two.
pub type Blend = [f32; 4];

/// Changes applied to particles over their life.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operator {
    /// Moves particles by their velocity. Without it, particles stay where
    /// they spawned. `drag` removes that fraction of velocity per second.
    Movement {
        #[serde(default)]
        gravity: [f32; 2],
        #[serde(default)]
        drag: f32,
    },
    /// Turns particles by their angular velocity.
    AngularMovement {
        #[serde(default)]
        force: [f32; 3],
        #[serde(default)]
        drag: f32,
    },
    /// Fades in until `fade_in` and out from `fade_out` (life fractions).
    AlphaFade {
        fade_in: f32,
        fade_out: f32,
    },
    /// Multiplies size from `value[0]` to `value[1]` between the two life
    /// fractions in `time`.
    SizeChange {
        time: [f32; 2],
        value: [f32; 2],
    },
    AlphaChange {
        time: [f32; 2],
        value: [f32; 2],
    },
    ColorChange {
        time: [f32; 2],
        start: [f32; 3],
        end: [f32; 3],
    },
    /// Multiplies alpha by a cosine between `scale[0]` and `scale[1]`.
    /// `frequency` counts oscillations per particle life.
    OscillateAlpha {
        frequency: [f32; 2],
        scale: [f32; 2],
        phase: [f32; 2],
    },
    OscillateSize {
        frequency: [f32; 2],
        scale: [f32; 2],
        phase: [f32; 2],
    },
    /// Sways particles around their path by up to `scale` pixels, per axis
    /// weighted by `mask`.
    OscillatePosition {
        frequency: [f32; 2],
        scale: [f32; 2],
        phase: [f32; 2],
        mask: [f32; 2],
        #[serde(default)]
        blend: Option<Blend>,
    },
    /// Pushes particles along a flowing noise field. `noise_scale` is the
    /// field's frequency per pixel, `noise_speed` how fast it changes.
    Turbulence {
        speed: [f32; 2],
        noise_scale: f32,
        noise_speed: f32,
        phase: [f32; 2],
        mask: [f32; 2],
        #[serde(default)]
        blend: Option<Blend>,
    },
}

/// How particles are drawn.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SpriteRenderer {
    /// Textured squares facing the screen, turned by their rotation.
    #[default]
    Sprite,
    /// Sprites stretched along their velocity: `length` times the speed,
    /// clamped to `min_length`..`max_length`, multiplies the sprite's height.
    Trail {
        length: f32,
        #[serde(default)]
        min_length: f32,
        max_length: f32,
    },
}

fn one() -> f32 {
    1.0
}
fn yes() -> bool {
    true
}
fn unit2() -> [f32; 2] {
    [1.0, 1.0]
}

// ---- randomness ------------------------------------------------------------

/// SplitMix64: small, fast and identical everywhere.
#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        mix(self.0)
    }

    /// Uniform in 0..1.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A per-particle random value in 0..1 for `slot` of operator `op`.
fn keyed(key: u64, op: usize, slot: u64) -> f64 {
    let h = mix(key ^ mix(((op as u64) << 8) | slot));
    (h >> 11) as f64 / (1u64 << 53) as f64
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn biased(t: f64, exponent: f32) -> f64 {
    if exponent == 1.0 {
        t
    } else {
        libm::pow(t, f64::from(exponent.max(0.0)))
    }
}

// ---- noise -----------------------------------------------------------------

/// Gradient noise in three dimensions, about -1..1.
fn noise3(p: [f64; 3]) -> f64 {
    let cell = p.map(libm::floor);
    let f = [p[0] - cell[0], p[1] - cell[1], p[2] - cell[2]];
    let fade = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let u = f.map(fade);
    let corner = |dx: i64, dy: i64, dz: i64| {
        let h = mix((cell[0] as i64 + dx) as u64
            ^ mix((cell[1] as i64 + dy) as u64 ^ mix((cell[2] as i64 + dz) as u64)));
        // Twelve gradient directions, as in improved Perlin noise.
        let (x, y, z) = (f[0] - dx as f64, f[1] - dy as f64, f[2] - dz as f64);
        match h % 12 {
            0 => x + y,
            1 => -x + y,
            2 => x - y,
            3 => -x - y,
            4 => x + z,
            5 => -x + z,
            6 => x - z,
            7 => -x - z,
            8 => y + z,
            9 => -y + z,
            10 => y - z,
            _ => -y - z,
        }
    };
    let x00 = lerp(corner(0, 0, 0), corner(1, 0, 0), u[0]);
    let x10 = lerp(corner(0, 1, 0), corner(1, 1, 0), u[0]);
    let x01 = lerp(corner(0, 0, 1), corner(1, 0, 1), u[0]);
    let x11 = lerp(corner(0, 1, 1), corner(1, 1, 1), u[0]);
    lerp(lerp(x00, x10, u[1]), lerp(x01, x11, u[1]), u[2])
}

/// Direction of a divergence-free flow at `p` (2D curl of the noise), or
/// zero where the field is flat.
fn curl2(p: [f64; 2], t: f64) -> [f64; 2] {
    const E: f64 = 1e-3;
    let n = |x: f64, y: f64| noise3([x, y, t]);
    let dy = (n(p[0], p[1] + E) - n(p[0], p[1] - E)) / (2.0 * E);
    let dx = (n(p[0] + E, p[1]) - n(p[0] - E, p[1])) / (2.0 * E);
    let v = [dy, -dx];
    let len = libm::sqrt(v[0] * v[0] + v[1] * v[1]);
    if len < 1e-9 {
        [0.0, 0.0]
    } else {
        [v[0] / len, v[1] / len]
    }
}

// ---- simulation ------------------------------------------------------------

#[derive(Clone)]
struct Particle {
    key: u64,
    pos: [f64; 2],
    vel: [f64; 2],
    rot: [f64; 3],
    spin: [f64; 3],
    age: f64,
    life: f64,
    size: f64,
    alpha: f64,
    color: [f64; 3],
}

#[derive(Clone, Default)]
struct EmitState {
    /// Fractional particles owed.
    owed: f64,
    burst_done: bool,
    last_spawn: Option<f64>,
}

/// A particle as drawn: everything the renderer needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sprite {
    /// Local position.
    pub pos: [f32; 2],
    pub vel: [f32; 2],
    pub rot: [f32; 3],
    /// Width in pixels.
    pub size: f32,
    pub color: [f32; 3],
    pub alpha: f32,
    /// Sprite-sheet frame and the next one, and how far to blend into it.
    pub frame: (u32, u32, f32),
}

/// The running state of one system.
pub struct Simulation {
    system: Particles,
    rng: Rng,
    particles: Vec<Particle>,
    emitters: Vec<EmitState>,
    /// Steps taken since the system started (including `start_time`).
    steps: u64,
}

impl Simulation {
    pub fn new(system: &Particles) -> Self {
        Self::starting_at(system, 0)
    }

    fn starting_at(system: &Particles, first_step: u64) -> Self {
        Self {
            rng: Rng(mix(system.seed ^ first_step)),
            particles: Vec::with_capacity(system.max_count.min(100_000) as usize),
            emitters: vec![EmitState::default(); system.emitters.len()],
            system: system.clone(),
            steps: first_step,
        }
    }

    /// System time in seconds: scene time plus `start_time`.
    fn time(&self) -> f64 {
        self.steps as f64 * STEP
    }

    /// Brings the system to scene time `scene_time`. Going back in time
    /// replays it from the start.
    pub fn advance_to(&mut self, scene_time: f64) {
        let target = libm::floor((scene_time.max(0.0) + f64::from(self.system.start_time)) / STEP);
        let target = target as u64;
        let far = (MAX_CATCH_UP / STEP) as u64;
        if target < self.steps {
            *self = Self::new(&self.system);
        }
        if target > far && target - self.steps > far {
            // Restart a fixed distance before the target, so the result
            // still depends only on the target.
            *self = Self::starting_at(&self.system, target - far);
        }
        while self.steps < target {
            self.step();
        }
    }

    fn step(&mut self) {
        let t = self.time();
        for i in 0..self.system.emitters.len() {
            self.emit(i, t);
        }
        let ops = std::mem::take(&mut self.system.operators);
        for (index, op) in ops.iter().enumerate() {
            for p in &mut self.particles {
                integrate(op, index, p, t);
            }
        }
        self.system.operators = ops;
        for p in &mut self.particles {
            p.age += STEP;
        }
        self.particles.retain(|p| p.age < p.life);
        self.steps += 1;
    }

    fn emit(&mut self, index: usize, t: f64) {
        let emitter = &self.system.emitters[index];
        let state = &mut self.emitters[index];
        let start = f64::from(emitter.delay);
        if t < start {
            return;
        }
        if emitter.duration > 0.0 && t >= start + f64::from(emitter.duration) {
            return;
        }
        let mut count = 0u32;
        if !state.burst_done {
            state.burst_done = true;
            // Only on the step the emitter starts: a run restarted after a
            // far jump has long passed it.
            if t - start < STEP {
                count += emitter.burst;
            }
        }
        state.owed += f64::from(emitter.rate.max(0.0)) * STEP;
        let due = libm::floor(state.owed);
        state.owed -= due;
        count += due as u32;
        if emitter.one_per_frame && count > 0 {
            if state.last_spawn.is_some_and(|last| t - last < FRAME - 1e-9) {
                count = 0;
            } else {
                count = 1;
            }
        }
        if count > 0 {
            state.last_spawn = Some(t);
        }
        let emitter = emitter.clone();
        for _ in 0..count {
            if self.particles.len() >= self.system.max_count as usize {
                break;
            }
            let p = self.spawn(&emitter, t);
            self.particles.push(p);
        }
    }

    fn spawn(&mut self, emitter: &Emitter, t: f64) -> Particle {
        let rng = &mut self.rng;
        let mut offset = match emitter.shape {
            EmitterShape::Disk { radius } => {
                let (r0, r1) = (f64::from(radius[0]), f64::from(radius[1]));
                let r = libm::sqrt(lerp(r0 * r0, r1 * r1, rng.unit()));
                let a = rng.unit() * std::f64::consts::TAU;
                [r * libm::cos(a), r * libm::sin(a)]
            }
            EmitterShape::Box { min, max } => {
                let mut axis = |i: usize| {
                    let d = lerp(f64::from(min[i]), f64::from(max[i]), rng.unit());
                    if rng.unit() < 0.5 { -d } else { d }
                };
                [axis(0), axis(1)]
            }
        };
        for ((o, scale), sign) in offset.iter_mut().zip(emitter.scale).zip(emitter.sign) {
            *o *= f64::from(scale);
            match sign {
                1 => *o = o.abs(),
                -1 => *o = -o.abs(),
                _ => {}
            }
        }
        let mut vel = [0.0; 2];
        if emitter.speed[1] > 0.0 || emitter.speed[0] != 0.0 {
            let len = libm::sqrt(offset[0] * offset[0] + offset[1] * offset[1]);
            let dir = if len > 1e-9 {
                [offset[0] / len, offset[1] / len]
            } else {
                [0.0, -1.0]
            };
            let s = lerp(
                f64::from(emitter.speed[0]),
                f64::from(emitter.speed[1]),
                rng.unit(),
            );
            vel = [dir[0] * s, dir[1] * s];
        }
        let mut p = Particle {
            key: rng.next(),
            pos: [
                f64::from(emitter.offset[0]) + offset[0],
                f64::from(emitter.offset[1]) + offset[1],
            ],
            vel,
            rot: [0.0; 3],
            spin: [0.0; 3],
            age: 0.0,
            life: 1.0,
            size: 20.0,
            alpha: 1.0,
            color: [1.0; 3],
        };
        for init in &self.system.initializers {
            initialize(init, &mut p, rng, t);
        }
        p
    }

    /// The live particles as drawn now, oldest first.
    pub fn sprites(&self, out: &mut Vec<Sprite>) {
        out.clear();
        for p in &self.particles {
            let mut sprite = appearance(&self.system.operators, p);
            if let Some(sheet) = &self.system.sheet {
                sprite.frame = sheet_frame(sheet, p);
            }
            out.push(sprite);
        }
    }

    /// Particles alive.
    pub fn len(&self) -> usize {
        self.particles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.particles.is_empty()
    }

    pub fn system(&self) -> &Particles {
        &self.system
    }
}

fn initialize(init: &Initializer, p: &mut Particle, rng: &mut Rng, t: f64) {
    let draw = |rng: &mut Rng, min: f32, max: f32, exponent: f32| {
        lerp(f64::from(min), f64::from(max), biased(rng.unit(), exponent))
    };
    match *init {
        Initializer::Lifetime { min, max, exponent } => p.life = draw(rng, min, max, exponent),
        Initializer::Size { min, max, exponent } => p.size = draw(rng, min, max, exponent),
        Initializer::Alpha { min, max, exponent } => p.alpha = draw(rng, min, max, exponent),
        Initializer::Color { min, max, exponent } => {
            let k = biased(rng.unit(), exponent);
            p.color = std::array::from_fn(|i| lerp(f64::from(min[i]), f64::from(max[i]), k));
        }
        Initializer::Velocity { min, max, exponent } => {
            for i in 0..2 {
                p.vel[i] += draw(rng, min[i], max[i], exponent);
            }
        }
        Initializer::Rotation { min, max, exponent } => {
            p.rot = std::array::from_fn(|i| draw(rng, min[i], max[i], exponent));
        }
        Initializer::AngularVelocity { min, max, exponent } => {
            p.spin = std::array::from_fn(|i| draw(rng, min[i], max[i], exponent));
        }
        Initializer::TurbulentVelocity {
            speed,
            scale,
            offset,
            forward,
            noise_speed,
            phase,
        } => {
            let phase = draw(rng, phase[0], phase[1], 1.0);
            let s = draw(rng, speed[0], speed[1], 1.0);
            // Smooth in time, so particles spawned close together travel
            // close together.
            let n = noise3([t * f64::from(noise_speed) + phase, 0.37, 0.71]).clamp(-1.0, 1.0);
            let base = libm::atan2(f64::from(forward[1]), f64::from(forward[0]));
            let a = base + n * f64::from(scale) * std::f64::consts::PI + f64::from(offset);
            p.vel[0] += libm::cos(a) * s;
            p.vel[1] += libm::sin(a) * s;
        }
    }
}

/// Fraction of life in 0..1.
fn life_fraction(p: &Particle) -> f64 {
    if p.life > 0.0 {
        (p.age / p.life).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

fn window(blend: Option<Blend>, life: f64) -> f64 {
    let Some([a, b, c, d]) = blend.map(|b| b.map(f64::from)) else {
        return 1.0;
    };
    let ramp = |x: f64, from: f64, to: f64| {
        if to <= from {
            if x >= from { 1.0 } else { 0.0 }
        } else {
            ((x - from) / (to - from)).clamp(0.0, 1.0)
        }
    };
    ramp(life, a, b) * (1.0 - ramp(life, c, d))
}

/// Applies an operator's effect on motion for one step.
fn integrate(op: &Operator, index: usize, p: &mut Particle, t: f64) {
    match *op {
        Operator::Movement { gravity, drag } => {
            for (i, g) in gravity.iter().enumerate() {
                p.pos[i] += p.vel[i] * STEP;
                p.vel[i] += f64::from(*g) * STEP;
            }
            let keep = (1.0 - f64::from(drag) * STEP).max(0.0);
            p.vel = p.vel.map(|v| v * keep);
        }
        Operator::AngularMovement { force, drag } => {
            for (i, f) in force.iter().enumerate() {
                p.rot[i] += p.spin[i] * STEP;
                p.spin[i] += f64::from(*f) * STEP;
            }
            let keep = (1.0 - f64::from(drag) * STEP).max(0.0);
            p.spin = p.spin.map(|v| v * keep);
        }
        Operator::Turbulence {
            speed,
            noise_scale,
            noise_speed,
            phase,
            mask,
            blend,
        } => {
            let w = window(blend, life_fraction(p));
            if w == 0.0 {
                return;
            }
            let s = lerp(
                f64::from(speed[0]),
                f64::from(speed[1]),
                keyed(p.key, index, 0),
            );
            let ph = lerp(
                f64::from(phase[0]),
                f64::from(phase[1]),
                keyed(p.key, index, 1),
            );
            let k = f64::from(noise_scale);
            let dir = curl2(
                [p.pos[0] * k, p.pos[1] * k],
                ph + t * f64::from(noise_speed),
            );
            for i in 0..2 {
                p.vel[i] += dir[i] * s * f64::from(mask[i]) * w * STEP;
            }
        }
        _ => {}
    }
}

/// Evaluates the operators that depend only on a particle's age.
fn appearance(ops: &[Operator], p: &Particle) -> Sprite {
    let life = life_fraction(p);
    let (mut size, mut alpha, mut color) = (p.size, p.alpha, p.color);
    let mut pos = p.pos;
    let tau = std::f64::consts::TAU;
    let ramp = |life: f64, time: [f32; 2], from: f64, to: f64| {
        let (a, b) = (f64::from(time[0]), f64::from(time[1]));
        if life <= a {
            from
        } else if life >= b || b <= a {
            to
        } else {
            lerp(from, to, (life - a) / (b - a))
        }
    };
    for (index, op) in ops.iter().enumerate() {
        // Cosine wave between `scale`s, `frequency` times per life.
        let wave = |frequency: [f32; 2], phase: [f32; 2], slot: u64| {
            let f = lerp(
                f64::from(frequency[0]),
                f64::from(frequency[1]),
                keyed(p.key, index, slot),
            );
            let ph = lerp(
                f64::from(phase[0]),
                f64::from(phase[1]),
                keyed(p.key, index, slot + 1),
            );
            (ph, tau * f * life + ph)
        };
        match *op {
            Operator::AlphaFade { fade_in, fade_out } => {
                let (fi, fo) = (f64::from(fade_in), f64::from(fade_out));
                if life < fi {
                    alpha *= life / fi;
                } else if life > fo && fo < 1.0 {
                    alpha *= 1.0 - (life - fo) / (1.0 - fo);
                }
            }
            Operator::SizeChange { time, value } => {
                size *= ramp(life, time, f64::from(value[0]), f64::from(value[1]));
            }
            Operator::AlphaChange { time, value } => {
                alpha *= ramp(life, time, f64::from(value[0]), f64::from(value[1]));
            }
            Operator::ColorChange { time, start, end } => {
                for i in 0..3 {
                    color[i] *= ramp(life, time, f64::from(start[i]), f64::from(end[i]));
                }
            }
            Operator::OscillateAlpha {
                frequency,
                scale,
                phase,
            } => {
                let (_, a) = wave(frequency, phase, 0);
                let k = (libm::cos(a) + 1.0) * 0.5;
                alpha *= lerp(f64::from(scale[0]), f64::from(scale[1]), k);
            }
            Operator::OscillateSize {
                frequency,
                scale,
                phase,
            } => {
                let (_, a) = wave(frequency, phase, 0);
                let k = (libm::cos(a) + 1.0) * 0.5;
                size *= lerp(f64::from(scale[0]), f64::from(scale[1]), k);
            }
            Operator::OscillatePosition {
                frequency,
                scale,
                phase,
                mask,
                blend,
            } => {
                let w = window(blend, life);
                for i in 0..2 {
                    let slot = 4 * i as u64;
                    let (ph, a) = wave(frequency, phase, slot);
                    let amp = lerp(
                        f64::from(scale[0]),
                        f64::from(scale[1]),
                        keyed(p.key, index, slot + 2),
                    );
                    // Starts at rest on the particle's path.
                    pos[i] += amp * (libm::cos(a) - libm::cos(ph)) * f64::from(mask[i]) * w;
                }
            }
            _ => {}
        }
    }
    Sprite {
        pos: pos.map(|v| v as f32),
        vel: p.vel.map(|v| v as f32),
        rot: p.rot.map(|v| v as f32),
        size: size.max(0.0) as f32,
        color: color.map(|v| v as f32),
        alpha: alpha.clamp(0.0, 1.0) as f32,
        frame: (0, 0, 0.0),
    }
}

/// Picks the sprite-sheet frames for a particle, as Wallpaper Engine's
/// particle shader does: the next frame does not wrap around.
fn sheet_frame(sheet: &SpriteSheet, p: &Particle) -> (u32, u32, f32) {
    let n = sheet.frames.len().max(1) as f64;
    let last = n - 1.0;
    let life = life_fraction(p) * f64::from(sheet.speed);
    let at = match sheet.mode {
        SheetMode::Sequence => (life - libm::floor(life)) * n,
        SheetMode::Once => (life * n).min(last),
        SheetMode::Random => libm::floor(keyed(p.key, usize::MAX, 0) * n),
    };
    let current = libm::floor(at).clamp(0.0, last);
    let next = (current + 1.0).min(last);
    let blend = if sheet.blend && sheet.mode != SheetMode::Random {
        (at - current) as f32
    } else {
        0.0
    };
    (current as u32, next as u32, blend)
}

/// The sprite's horizontal and vertical texture axes in local space (y down)
/// for rotation `rot`, as Wallpaper Engine's particle shader builds them:
/// rotate about z, then x, then y. A pure z rotation turns the sprite
/// clockwise on screen.
pub fn sprite_axes(rot: [f32; 3]) -> ([f32; 2], [f32; 2]) {
    let (sx, cx) = libm::sincosf(rot[0]);
    let (sy, cy) = libm::sincosf(rot[1]);
    let (sz, cz) = libm::sincosf(rot[2]);
    // Row-vector convention, y up: rows of Rz·Rx·Ry.
    let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let mul = |a: [[f32; 3]; 3], b: [[f32; 3]; 3]| {
        let mut m = [[0.0f32; 3]; 3];
        for (i, row) in m.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
            }
        }
        m
    };
    let m = mul(mul(rz, rx), ry);
    let (right, up) = (m[0], m[1]);
    // Mirror into y-down space; the texture's v axis runs against "up".
    ([right[0], -right[1]], [-up[0], up[1]])
}

/// The quad of a sprite in local space: center, and the vectors along the
/// texture's u and v axes spanning the full width and height. `aspect` is
/// the texture's height over its width.
pub fn sprite_quad(
    sprite: &Sprite,
    renderer: &SpriteRenderer,
    aspect: f32,
) -> ([f32; 2], [f32; 2], [f32; 2]) {
    let w = sprite.size;
    match *renderer {
        SpriteRenderer::Sprite => {
            let (u, v) = sprite_axes(sprite.rot);
            (
                sprite.pos,
                [u[0] * w, u[1] * w],
                [v[0] * w * aspect, v[1] * w * aspect],
            )
        }
        SpriteRenderer::Trail {
            length,
            min_length,
            max_length,
        } => {
            let speed = (sprite.vel[0] * sprite.vel[0] + sprite.vel[1] * sprite.vel[1]).sqrt();
            if speed < 1e-6 {
                return (sprite.pos, [0.0; 2], [0.0; 2]);
            }
            let dir = [sprite.vel[0] / speed, sprite.vel[1] / speed];
            let stretch = (speed * length).min(max_length).max(min_length);
            // Width across the motion; the texture's top leads.
            let across = [-dir[1], dir[0]];
            let h = w * aspect * stretch;
            (
                sprite.pos,
                [across[0] * w, across[1] * w],
                [-dir[0] * h, -dir[1] * h],
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embers() -> Particles {
        Particles {
            max_count: 40,
            start_time: 3.0,
            seed: 7,
            emitters: vec![Emitter {
                shape: EmitterShape::Disk {
                    radius: [0.0, 32.0],
                },
                offset: [0.0, 0.0],
                scale: [1.0, 1.0],
                sign: [0, 0],
                rate: 5.0,
                burst: 0,
                delay: 0.0,
                duration: 0.0,
                speed: [0.0, 0.0],
                one_per_frame: false,
            }],
            initializers: vec![
                Initializer::Lifetime {
                    min: 3.0,
                    max: 5.0,
                    exponent: 1.0,
                },
                Initializer::Size {
                    min: 10.0,
                    max: 25.0,
                    exponent: 1.0,
                },
                Initializer::TurbulentVelocity {
                    speed: [100.0, 250.0],
                    scale: 0.1,
                    offset: 0.5,
                    forward: [0.0, -1.0],
                    noise_speed: 1.0,
                    phase: [0.0, 0.1],
                },
            ],
            operators: vec![
                Operator::Movement {
                    gravity: [0.0, 0.0],
                    drag: 0.0,
                },
                Operator::AlphaFade {
                    fade_in: 0.1,
                    fade_out: 1.0,
                },
                Operator::Turbulence {
                    speed: [100.0, 150.0],
                    noise_scale: 0.004,
                    noise_speed: 20.0,
                    phase: [0.0, 5.0],
                    mask: [1.0, 0.0],
                    blend: None,
                },
                Operator::OscillateAlpha {
                    frequency: [5.0, 20.0],
                    scale: [0.2, 1.0],
                    phase: [0.0, std::f32::consts::TAU],
                },
            ],
            renderer: SpriteRenderer::Trail {
                length: 0.007,
                min_length: 0.0,
                max_length: 10.0,
            },
            sheet: None,
        }
    }

    fn frame(sim: &mut Simulation, t: f64) -> Vec<Sprite> {
        sim.advance_to(t);
        let mut out = Vec::new();
        sim.sprites(&mut out);
        out
    }

    #[test]
    fn same_time_same_particles() {
        let system = embers();
        let mut a = Simulation::new(&system);
        let mut b = Simulation::new(&system);
        // Different paths to the same time: steps of different sizes, and a
        // detour into the future and back.
        for t in [0.5, 1.0, 1.7, 2.0] {
            frame(&mut a, t);
        }
        frame(&mut b, 9.0);
        assert_eq!(frame(&mut a, 2.0), frame(&mut b, 2.0));
        assert!(!a.is_empty(), "start_time fills the system before t = 0");
    }

    #[test]
    fn far_jumps_depend_only_on_the_target() {
        let system = embers();
        let mut a = Simulation::new(&system);
        let mut b = Simulation::new(&system);
        frame(&mut a, 2.0);
        assert_eq!(frame(&mut a, 3600.0), frame(&mut b, 3600.0));
    }

    #[test]
    fn respects_max_count_and_lifetime() {
        let mut system = embers();
        system.emitters[0].rate = 1000.0;
        let mut sim = Simulation::new(&system);
        for t in [0.0, 0.5, 4.0, 10.0] {
            let sprites = frame(&mut sim, t);
            assert!(sprites.len() <= 40);
        }
        system.emitters[0].duration = 0.5;
        system.start_time = 0.0;
        let mut sim = Simulation::new(&system);
        assert!(!frame(&mut sim, 0.4).is_empty());
        // Every particle lives at most 5 s.
        assert!(frame(&mut sim, 6.0).is_empty());
    }

    #[test]
    fn rising_embers_rise() {
        let system = embers();
        let mut sim = Simulation::new(&system);
        let sprites = frame(&mut sim, 10.0);
        let mean_y: f32 = sprites.iter().map(|s| s.pos[1]).sum::<f32>() / sprites.len() as f32;
        assert!(
            mean_y < -100.0,
            "forward points up (y down): mean y {mean_y}"
        );
        for s in &sprites {
            assert!((0.0..=1.0).contains(&s.alpha));
        }
    }

    #[test]
    fn alpha_fade_and_size_change() {
        let p = Particle {
            key: 1,
            pos: [0.0; 2],
            vel: [0.0; 2],
            rot: [0.0; 3],
            spin: [0.0; 3],
            age: 1.0,
            life: 4.0,
            size: 10.0,
            alpha: 1.0,
            color: [1.0; 3],
        };
        let ops = [
            Operator::AlphaFade {
                fade_in: 0.5,
                fade_out: 0.9,
            },
            Operator::SizeChange {
                time: [0.0, 1.0],
                value: [1.0, 0.0],
            },
        ];
        let s = appearance(&ops, &p);
        assert!((s.alpha - 0.5).abs() < 1e-6, "{}", s.alpha);
        assert!((s.size - 7.5).abs() < 1e-5, "{}", s.size);
    }

    #[test]
    fn sprite_axes_turn_clockwise() {
        let (u, v) = sprite_axes([0.0; 3]);
        assert_eq!((u, v), ([1.0, 0.0], [0.0, 1.0]));
        let (u, _) = sprite_axes([0.0, 0.0, std::f32::consts::FRAC_PI_2]);
        // x turns toward +y: down on screen, clockwise.
        assert!(u[0].abs() < 1e-6 && (u[1] - 1.0).abs() < 1e-6, "{u:?}");
        // Tipping about x foreshortens the vertical axis.
        let (_, v) = sprite_axes([1.0, 0.0, 0.0]);
        assert!((v[1] - 1f32.cos()).abs() < 1e-6, "{v:?}");
    }

    #[test]
    fn trails_stretch_along_motion() {
        let sprite = Sprite {
            pos: [0.0; 2],
            vel: [0.0, -200.0],
            rot: [0.0; 3],
            size: 10.0,
            color: [1.0; 3],
            alpha: 1.0,
            frame: (0, 0, 0.0),
        };
        let renderer = SpriteRenderer::Trail {
            length: 0.01,
            min_length: 0.0,
            max_length: 10.0,
        };
        let (_, u, v) = sprite_quad(&sprite, &renderer, 1.0);
        assert!((u[0].abs() - 10.0).abs() < 1e-5);
        // 200 px/s × 0.01 = 2× the width, along y.
        assert!((v[1] - 20.0).abs() < 1e-4, "{v:?}");
    }

    #[test]
    fn sheet_frames_follow_life() {
        let sheet = SpriteSheet {
            frames: vec![[0.0, 0.0, 0.5, 0.5]; 4],
            mode: SheetMode::Sequence,
            speed: 1.0,
            blend: true,
        };
        let mut p = Particle {
            key: 3,
            pos: [0.0; 2],
            vel: [0.0; 2],
            rot: [0.0; 3],
            spin: [0.0; 3],
            age: 0.5,
            life: 1.0,
            size: 1.0,
            alpha: 1.0,
            color: [1.0; 3],
        };
        // Half way through four frames: frame 2, about to blend into 3.
        assert_eq!(sheet_frame(&sheet, &p), (2, 3, 0.0));
        p.age = 0.99;
        let (current, next, _) = sheet_frame(&sheet, &p);
        assert_eq!((current, next), (3, 3), "the last frame does not wrap");
        let random = SpriteSheet {
            mode: SheetMode::Random,
            ..sheet
        };
        let first = sheet_frame(&random, &p);
        p.age = 0.1;
        assert_eq!(sheet_frame(&random, &p), first, "fixed for the particle");
    }

    #[test]
    fn round_trips_as_json() {
        let system = embers();
        let json = serde_json::to_string(&system).unwrap();
        let back: Particles = serde_json::from_str(&json).unwrap();
        assert_eq!(back, system);
    }
}
