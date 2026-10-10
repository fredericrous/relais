//! Seeded SplitMix64 PRNG.
//!
//! Used to shuffle the training set between epochs of the learner's
//! warm start, so reproducibility is "record seed and settings", not
//! "hope the platform's RNG agrees". Not cryptographic; nothing that
//! must be unpredictable goes through it. Live trial assignment
//! (`route::trial`) draws from it, keyed by a stable hash, so a recorded
//! seed reproduces the draw.

pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, n)`. Biased-free enough for training order; not for
    /// cryptography.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        self.next_u64() % n
    }

    /// Inclusive Fisher-Yates: a uniform permutation under this seed.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            items.swap(i, j);
        }
    }
}

/// The exploration draw (learning-loop Phase 4), defined once for Rust
/// and the plugin's TypeScript: `u` is the first 8 bytes, big-endian, of
/// SHA-256 over the UTF-8 bytes of `seed`, `session` and `task`, each
/// preceded by its byte length as a 4-byte big-endian integer; the first
/// output `x` of [`SplitMix64`] seeded with `u`; the draw is
/// `(x >> 11) * 2^-53`, a uniform in `[0, 1)`.
pub fn exploration_draw(seed: &str, session: &str, task: &str) -> f64 {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in [seed, session, task] {
        hasher.update((part.len() as u32).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    let x = SplitMix64::new(u64::from_be_bytes(head)).next_u64();
    (x >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shared with `claude-plugin/tests/router.test.ts`: the same inputs
    /// must give the same draws in both languages.
    const VECTOR_INPUTS: [(&str, &str, &str); 4] = [
        ("0123456789abcdef", "session-1", "task-1"),
        ("fedcba9876543210", "sessión-é", "tâche-日本語-🚀"),
        ("", "", ""),
        ("a", "bc", "d"),
    ];

    /// Wrapping sum of the first 10,000 draws' 53-bit integers, and the
    /// draws below 0.1, for seed `seed-10k` and session `session-10k`.
    fn sequence_fingerprint() -> (u64, usize) {
        let mut sum = 0u64;
        let mut below = 0usize;
        for i in 0..10_000 {
            let draw = exploration_draw("seed-10k", "session-10k", &format!("task-{i}"));
            sum = sum.wrapping_add((draw * (1u64 << 53) as f64) as u64);
            if draw < 0.1 {
                below += 1;
            }
        }
        (sum, below)
    }

    #[test]
    fn the_exploration_draw_matches_the_shared_vector() {
        let expected = [
            0.24444899038069579,
            0.8532650270632014,
            0.96696972315229,
            0.34418990774171765,
        ];
        for ((seed, session, task), want) in VECTOR_INPUTS.iter().zip(expected) {
            assert_eq!(
                exploration_draw(seed, session, task),
                want,
                "{seed} {session} {task}"
            );
        }
    }

    #[test]
    fn ten_thousand_draws_match_the_shared_fingerprint_and_explore_ten_percent() {
        let (sum, below) = sequence_fingerprint();
        assert_eq!(sum, 8_148_712_194_814_526_056);
        assert_eq!(below, 1032, "10% +/- 1% of 10,000");
        assert!((900..=1100).contains(&below));
    }

    #[test]
    fn length_prefixes_keep_the_parts_apart() {
        assert_ne!(
            exploration_draw("a", "bc", "d"),
            exploration_draw("ab", "c", "d")
        );
    }

    #[test]
    fn same_seed_same_sequence() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = SplitMix64::new(1);
        let mut b = SplitMix64::new(2);
        let first = (a.next_u64(), a.next_u64());
        let second = (b.next_u64(), b.next_u64());
        assert_ne!(first, second);
    }

    #[test]
    fn shuffle_is_a_permutation() {
        let mut rng = SplitMix64::new(7);
        let mut items: Vec<u32> = (0..64).collect();
        rng.shuffle(&mut items);
        items.sort_unstable();
        assert_eq!(items, (0..64).collect::<Vec<u32>>());
    }

    #[test]
    fn shuffle_is_seed_reproducible() {
        let a = {
            let mut rng = SplitMix64::new(99);
            let mut v: Vec<u32> = (0..32).collect();
            rng.shuffle(&mut v);
            v
        };
        let b = {
            let mut rng = SplitMix64::new(99);
            let mut v: Vec<u32> = (0..32).collect();
            rng.shuffle(&mut v);
            v
        };
        assert_eq!(a, b);
    }

    #[test]
    fn below_stays_in_range() {
        let mut rng = SplitMix64::new(3);
        for _ in 0..64 {
            assert!(rng.below(10) < 10);
        }
    }

    proptest::proptest! {
        /// A shuffle rearranges; it never loses, duplicates or invents an
        /// element. The training order rests on this for every seed, not
        /// for the three a unit test happens to name.
        #[test]
        fn shuffle_is_a_permutation_for_any_seed_and_length(
            seed in proptest::num::u64::ANY,
            length in 0usize..64,
        ) {
            use proptest::prelude::*;
            let original: Vec<usize> = (0..length).collect();
            let mut shuffled = original.clone();
            SplitMix64::new(seed).shuffle(&mut shuffled);
            prop_assert_eq!(shuffled.len(), original.len());
            let mut sorted = shuffled.clone();
            sorted.sort_unstable();
            prop_assert_eq!(sorted, original, "same multiset, different order");
        }

        /// And it is a function of the seed alone: two runs of the same
        /// seed order the same list the same way, which is what makes a
        /// training split reproducible.
        #[test]
        fn a_seed_orders_the_same_list_the_same_way(
            seed in proptest::num::u64::ANY,
            length in 0usize..64,
        ) {
            use proptest::prelude::*;
            let mut first: Vec<usize> = (0..length).collect();
            let mut second = first.clone();
            SplitMix64::new(seed).shuffle(&mut first);
            SplitMix64::new(seed).shuffle(&mut second);
            prop_assert_eq!(first, second);
        }
    }
}
