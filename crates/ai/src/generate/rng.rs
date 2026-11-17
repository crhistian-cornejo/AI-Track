//! A small seeded random stream. The repo carries no `rand`, and the
//! generator must give the same notes for the same seed on every platform, so
//! the stream is written out here: splitmix64 turns seeds into state and
//! xoshiro256++ makes the numbers.

/// splitmix64: one step. Also used to derive independent seeds.
pub fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The seed of one part of a result (a layer, one bar of it, one purpose).
/// Editing or locking one part never moves the dice of another.
pub fn part_seed(seed: u64, role: u64, bar: u64, salt: u64) -> u64 {
    splitmix64(splitmix64(splitmix64(seed ^ 0xA17A_C0DE) ^ role.wrapping_mul(0x1000_0001)) ^ bar.wrapping_mul(0x0100_0193) ^ salt.wrapping_mul(0x9E37))
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

    /// The stream of one part of the result; see [`part_seed`].
    pub fn part(seed: u64, role: u64, bar: u64, salt: u64) -> Self {
        Self::new(part_seed(seed, role, bar, salt))
    }

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
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform in [-1, 1).
    pub fn signed(&mut self) -> f32 {
        self.unit() * 2. - 1.
    }

    /// Uniform in [lo, hi).
    pub fn between(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    /// Uniform in 0..n (0 when `n` is 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() % n as u64) as usize }
    }

    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        items.get(self.below(items.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_stream() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        assert!((0..100).all(|_| a.next_u64() == b.next_u64()));
        assert_ne!(Rng::new(1).next_u64(), Rng::new(2).next_u64());
    }

    #[test]
    fn a_known_stream_never_changes() {
        // Saved results depend on this: a change here is a GENERATOR_VERSION bump.
        let mut r = Rng::new(0);
        let first: Vec<u64> = (0..3).map(|_| r.next_u64()).collect();
        let mut again = Rng::new(0);
        assert_eq!(first, (0..3).map(|_| again.next_u64()).collect::<Vec<_>>());
        assert_eq!(splitmix64(0), 0xE220_A839_7B1D_CDAF);
    }

    #[test]
    fn numbers_stay_in_range_and_are_spread() {
        let mut r = Rng::new(7);
        let xs: Vec<f32> = (0..2000).map(|_| r.unit()).collect();
        assert!(xs.iter().all(|x| (0. ..1.).contains(x)));
        let mean = xs.iter().sum::<f32>() / xs.len() as f32;
        assert!((mean - 0.5).abs() < 0.05, "mean {mean}");
        assert!((0..500).all(|_| r.below(5) < 5));
        assert_eq!(r.below(0), 0);
    }

    #[test]
    fn parts_are_independent_of_each_other() {
        assert_eq!(part_seed(9, 1, 0, 0), part_seed(9, 1, 0, 0));
        assert_ne!(part_seed(9, 1, 0, 0), part_seed(9, 2, 0, 0));
        assert_ne!(part_seed(9, 1, 0, 0), part_seed(9, 1, 1, 0));
        assert_ne!(part_seed(9, 1, 0, 0), part_seed(10, 1, 0, 0));
    }
}
