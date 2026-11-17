use super::*;

const MAX_PARTICLES: usize = 30000;

/// The dots of deleted tiles, drifting up on the lattice and dimming level by level.
pub struct Particles {
    list: Vec<Particle>,
    rng: u64,
}

struct Particle {
    pos: Pos2,
    vel: Vec2,
    life: f32,
    max: f32,
    delay: f32,
    col: [u8; 3],
    /// Level it starts at (of the pixel field's 5).
    level: f32,
}

impl Default for Particles {
    fn default() -> Self {
        Self { list: vec![], rng: 0x2545f4914f6cdd1d }
    }
}

impl Particles {
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn clear(&mut self) {
        self.list.clear();
    }

    fn rand(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Dissolve a rect into its dots, rising and dimming, right edge first.
    pub fn burst(&mut self, r: Rect, col: [u8; 3], pitch: f32) {
        // Whole dots of the lattice, thinned out on big tiles to stay within MAX_PARTICLES.
        let step = ((r.area() / (pitch * pitch) / 12000.0).sqrt().ceil().max(1.0)) * pitch;
        let mut y = r.min.y;
        while y < r.max.y {
            let mut x = r.min.x;
            while x < r.max.x {
                if self.rand() < 0.7 && self.list.len() < MAX_PARTICLES {
                    let (a, b, c, d) = (self.rand(), self.rand(), self.rand(), self.rand());
                    self.list.push(Particle {
                        pos: pos2(x, y),
                        vel: vec2((a - 0.5) * 50.0, -20.0 - b * 90.0),
                        life: 0.6 + c * 1.2,
                        max: 0.6 + c * 1.2,
                        delay: (r.max.x - x) / r.width().max(1.0) * 0.5,
                        col,
                        level: if d < 0.5 { 4.0 } else { 3.0 },
                    });
                }
                x += step;
            }
            y += step;
        }
    }

    /// Move them on by `dt` seconds; the dot cells they are over now.
    pub fn step(&mut self, dt: f32, pitch: f32) -> Vec<Inst> {
        self.list.retain_mut(|p| {
            if p.delay > 0.0 {
                p.delay -= dt;
                return true;
            }
            p.life -= dt;
            p.vel.y -= 60.0 * dt;
            p.vel.x *= 1.0 - dt;
            p.pos += p.vel * dt;
            p.life > 0.0
        });
        let live = self.list.iter().filter(|p| p.delay <= 0.0);
        live.map(|p| {
            // At a level stepping down as it dies.
            let level = (p.level * (p.life / p.max).clamp(0.0, 1.0)).ceil();
            let c = (p.pos.to_vec2() / pitch).floor() * pitch;
            Inst { rect: [c.x, c.y, c.x + pitch, c.y + pitch], color: gfx::rgba(p.col, level / 4.0), p: [gfx::PARTICLE, gfx::NO_LABEL, 0, 0], q: [0; 4] }
        })
        .collect()
    }
}
