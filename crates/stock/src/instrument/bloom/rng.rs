//! The seeded random stream Bloom grows scenes from: splitmix64 for seeds,
//! xoshiro256++ for numbers. Arithmetic only (no `ln`, no `cos`), so a seed
//! gives the same scene on every machine.

/// splitmix64: one step. Also derives independent seeds.
#[inline]
pub fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The seed of one part of a result (a cell, a purpose).
#[inline]
pub fn part(seed: u64, cell: u64, salt: u64) -> u64 {
    splitmix64(splitmix64(seed ^ 0xB100_0B10_0B10_0B10) ^ cell.wrapping_mul(0x1000_0001_0000_0003) ^ salt.wrapping_mul(0x9E37_79B9))
}

#[derive(Clone, Debug)]
pub struct Rng {
    s: [u64; 4],
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut x = seed;
        let mut s = [0u64; 4];
        for slot in &mut s {
            x = splitmix64(x);
            *slot = x;
        }
        Self { s }
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let r = self.s[0].wrapping_add(self.s[3]).rotate_left(23).wrapping_add(self.s[0]);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        r
    }

    /// Uniform in [0, 1).
    #[inline]
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform in [lo, hi).
    #[inline]
    pub fn between(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    /// Uniform in 0..n (0 when `n` is 0).
    #[inline]
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() % n as u64) as usize }
    }

    #[inline]
    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }

    /// Roughly normal, mean 0 and deviation 1 (Irwin–Hall of four uniforms,
    /// so no transcendental functions are involved).
    #[inline]
    pub fn normal(&mut self) -> f32 {
        let s = self.unit() + self.unit() + self.unit() + self.unit();
        (s - 2.0) * 1.732_050_8
    }

    pub fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_are_reproducible_and_spread() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        assert!((0..100).all(|_| a.next_u64() == b.next_u64()));
        let mut r = Rng::new(3);
        let xs: Vec<f32> = (0..4000).map(|_| r.normal()).collect();
        let mean = xs.iter().sum::<f32>() / xs.len() as f32;
        let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / xs.len() as f32;
        assert!(mean.abs() < 0.06 && (var - 1.0).abs() < 0.08, "{mean} {var}");
        assert_ne!(part(1, 0, 0), part(1, 1, 0));
        assert_ne!(part(1, 0, 0), part(1, 0, 1));
        // A known value: saved scenes depend on it.
        assert_eq!(splitmix64(0), 0xE220_A839_7B1D_CDAF);
    }
}
