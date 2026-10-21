//! xorshift64*: enough randomness for a CPU batter or a mock event, and
//! reproducible from a seed in tests.

pub struct Rng(u64);

impl Rng {
    pub fn seeded() -> Self {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        Self::with_seed(t)
    }

    pub fn with_seed(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    pub fn between(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    /// Roughly normal, mean 0, standard deviation about 1.
    pub fn normal(&mut self) -> f64 {
        (self.unit() + self.unit() + self.unit() - 1.5) * 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_in_range_and_repeats_from_a_seed() {
        let mut a = Rng::with_seed(42);
        let mut b = Rng::with_seed(42);
        for _ in 0..1000 {
            let x = a.unit();
            assert!((0.0..1.0).contains(&x));
            assert_eq!(x, b.unit());
            let y = a.between(5.0, 6.0);
            assert!((5.0..6.0).contains(&y));
            b.between(5.0, 6.0);
        }
        let mean: f64 = (0..2000).map(|_| a.normal()).sum::<f64>() / 2000.0;
        assert!(mean.abs() < 0.1, "{mean}");
    }
}
