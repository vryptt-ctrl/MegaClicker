//! Tiny deterministic PRNG for interval and cursor jitter.
//!
//! MegaClicker ships no external random-number dependency and does not need
//! cryptographic quality. This xorshift stream only has to break up perfectly
//! uniform output; it is seeded once per session from the wall clock.

use std::time::{SystemTime, UNIX_EPOCH};

/// An xorshift64* generator. The state is never zero, so the stream is stable.
#[derive(Debug, Clone, Copy)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Create a generator from a seed. Zero is remapped because xorshift
    /// cannot recover from an all-zero state.
    pub fn seeded(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    /// Next value in the stream.
    pub fn next_u64(&mut self) -> u64 {
        let mut state = self.state;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.state = state;

        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform value in `0..bound`, or 0 when `bound` is 0.
    pub fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            0
        } else {
            self.next_u64() % bound
        }
    }
}

/// Seed for a new session. Mixing in the process id keeps two instances started
/// in the same millisecond from sharing a stream.
pub fn seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);

    nanos ^ u64::from(std::process::id()).rotate_left(32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_repeats_and_different_seeds_diverge() {
        let mut first = Rng::seeded(42);
        let mut second = Rng::seeded(42);
        let mut other = Rng::seeded(43);

        let first_values: Vec<u64> = (0..8).map(|_| first.next_u64()).collect();
        let second_values: Vec<u64> = (0..8).map(|_| second.next_u64()).collect();
        let other_values: Vec<u64> = (0..8).map(|_| other.next_u64()).collect();

        assert_eq!(first_values, second_values);
        assert_ne!(first_values, other_values);
    }

    #[test]
    fn below_stays_inside_the_bound() {
        let mut rng = Rng::seeded(7);

        for bound in [1, 2, 3, 7, 51, 201] {
            for _ in 0..100 {
                assert!(rng.below(bound) < bound);
            }
        }

        assert_eq!(rng.below(0), 0, "a zero bound must not panic");
    }

    #[test]
    fn zero_seed_is_remapped_to_a_live_stream() {
        let mut rng = Rng::seeded(0);
        let values: Vec<u64> = (0..4).map(|_| rng.next_u64()).collect();

        assert!(values.iter().any(|value| *value != 0));
    }
}
