//! The shader's uniform block, field for field as `struct U` in
//! `matter.wgsl` (every field a `vec4f` or an array of them, so the layout
//! has no padding).

use super::frame::VOICES;

/// What one frame of the matter is drawn from.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Uniforms {
    /// Width, height (px), time (s), seed.
    pub res: [f32; 4],
    /// Centre x, centre y (px), scale (px per unit), corner radius (px).
    pub frame: [f32; 4],
    /// Panel colour (linear), light theme (0/1).
    pub bg: [f32; 4],
    /// Colour outside the rounded corners (sRGB-encoded; the NV12 passes only).
    pub outer: [f32; 4],
    /// Size, swell, breath rate, churn.
    pub shape0: [f32; 4],
    /// Lumps, fold frequency, wobble frequency, wobble amount.
    pub shape1: [f32; 4],
    /// Spike amount, sharpness, count, shudder.
    pub spikes: [f32; 4],
    /// Pointer x, y (units), pull (+ attract, − repel), presence 0..1.
    pub cursor: [f32; 4],
    /// Albedo (linear), metal.
    pub mat0: [f32; 4],
    /// Roughness, clearcoat, iridescence, film thickness (µm).
    pub mat1: [f32; 4],
    /// Film range (µm), film shift (µm), rim, glass.
    pub mat2: [f32; 4],
    /// Emission colour (linear), emission amount.
    pub mat3: [f32; 4],
    /// Diffuse wrap, exposure, smoke tint, environment warmth.
    pub mat4: [f32; 4],
    /// Smoke amount, puff, rise speed, turbulence.
    pub smoke0: [f32; 4],
    pub smoke_a: [f32; 4],
    pub smoke_b: [f32; 4],
    pub smoke_c: [f32; 4],
    /// The sound's colour (linear), how much it tints the studio.
    pub tint: [f32; 4],
    /// Output level (smoothed), the voices' summed level, the droplets'
    /// reach from the centre (units), 1 while a ripple or a droplet lives.
    pub glow: [f32; 4],
    /// Each voice's colour (linear) and level (0 = no voice).
    pub voice: [[f32; 4]; VOICES],
    /// Each voice's place on the mass (unit direction) and seconds since its
    /// hit (< 0: no ripple).
    pub vdir: [[f32; 4]; VOICES],
    /// Each voice's droplet: centre (units) and radius (0 = none).
    pub drop: [[f32; 4]; VOICES],
    /// How hard each voice's last hit was.
    pub kick: [f32; 4],
}

/// Size of the block in bytes.
pub const SIZE: usize = std::mem::size_of::<Uniforms>();

impl Uniforms {
    /// The block as bytes, for `Queue::write_buffer`.
    pub fn bytes(&self) -> [u8; SIZE] {
        let mut out = [0u8; SIZE];
        let floats = self.floats();
        for (chunk, v) in out.as_chunks_mut::<4>().0.iter_mut().zip(floats.iter()) {
            chunk.copy_from_slice(&v.to_ne_bytes());
        }
        out
    }

    /// Every value in order.
    pub fn floats(&self) -> [f32; SIZE / 4] {
        let rows = [
            self.res, self.frame, self.bg, self.outer, self.shape0, self.shape1, self.spikes, self.cursor, self.mat0, self.mat1, self.mat2, self.mat3, self.mat4, self.smoke0, self.smoke_a,
            self.smoke_b, self.smoke_c, self.tint, self.glow,
        ];
        let mut out = [0f32; SIZE / 4];
        let all = rows.iter().chain(&self.voice).chain(&self.vdir).chain(&self.drop).chain(std::iter::once(&self.kick));
        for (i, r) in all.enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(r);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_matches_the_shader() {
        assert_eq!(SIZE, 32 * 16, "thirty-two vec4f");
        let wgsl = include_str!("matter.wgsl");
        let block = &wgsl[wgsl.find("struct U {").unwrap()..];
        let block = &block[..block.find("};").unwrap()];
        let arrays = block.matches("array<vec4f, 4>").count();
        assert_eq!(block.matches("vec4f").count() - arrays + arrays * VOICES, SIZE / 16);
        let mut u = Uniforms::default();
        u.kick[3] = 2.5;
        u.voice[0][3] = 0.75;
        let b = u.bytes();
        assert_eq!(f32::from_ne_bytes(b[SIZE - 4..].try_into().unwrap()), 2.5);
        assert_eq!(f32::from_ne_bytes(b[19 * 16 + 12..19 * 16 + 16].try_into().unwrap()), 0.75, "the voices follow the glow");
    }
}
