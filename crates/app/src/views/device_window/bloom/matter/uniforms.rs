//! The shader's uniform block, field for field as `struct U` in
//! `matter.wgsl` (every field a `vec4f`, so the layout has no padding).

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
            self.smoke_b, self.smoke_c, self.tint,
        ];
        let mut out = [0f32; SIZE / 4];
        for (i, r) in rows.iter().enumerate() {
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
        assert_eq!(SIZE, 18 * 16, "eighteen vec4f");
        let wgsl = include_str!("matter.wgsl");
        let block = &wgsl[wgsl.find("struct U {").unwrap()..];
        let block = &block[..block.find("};").unwrap()];
        assert_eq!(block.matches("vec4f").count(), SIZE / 16);
        let mut u = Uniforms::default();
        u.tint[3] = 2.5;
        let b = u.bytes();
        assert_eq!(f32::from_ne_bytes(b[SIZE - 4..].try_into().unwrap()), 2.5);
    }
}
