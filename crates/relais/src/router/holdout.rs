//! The per-session draw (plan §5): whether a session is held out, and the
//! seed its exploration draws come from. `SplitMix64` seeded by the first
//! eight bytes of SHA-256 of the session id, so the same session always
//! gets the same answer, in any process. Pure.

use sha2::{Digest, Sha256};

use crate::rng::SplitMix64;

/// What [`draw`] decided for one session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionDraw {
    pub holdout: bool,
    /// 16 lowercase hex characters.
    pub seed: String,
    /// The uniform the hold-out compared against the rate, in `[0, 1)`.
    pub uniform: f64,
}

/// The stream's seed: SHA-256 of the session id, first 8 bytes,
/// big-endian.
pub fn session_hash(session: &str) -> u64 {
    let digest = Sha256::digest(session.as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
}

/// A `u64` as a uniform in `[0, 1)`: its top 53 bits over 2^53.
pub fn unit(value: u64) -> f64 {
    (value >> 11) as f64 / (1u64 << 53) as f64
}

/// The first value of the session's stream decides the hold-out
/// (`uniform < holdout_rate`), the second is the session's seed.
pub fn draw(session: &str, holdout_rate: f64) -> SessionDraw {
    let mut rng = SplitMix64::new(session_hash(session));
    let uniform = unit(rng.next_u64());
    let seed = rng.next_u64();
    SessionDraw {
        holdout: uniform < holdout_rate,
        seed: format!("{seed:016x}"),
        uniform,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_session_always_draws_the_same() {
        let a = draw("session-abc", 0.1);
        let b = draw("session-abc", 0.1);
        assert_eq!(a, b);
        assert_eq!(a.seed.len(), 16);
        assert!(a.seed.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(draw("session-abd", 0.1).seed, a.seed);
    }

    /// The test vector the plugin's own implementation is held to: a
    /// change here is a change to the wire contract.
    #[test]
    fn the_test_vector_is_stable() {
        let hash = session_hash("s0");
        let mut rng = SplitMix64::new(hash);
        let first = rng.next_u64();
        let second = rng.next_u64();
        let drawn = draw("s0", 0.1);
        assert_eq!(drawn.seed, format!("{second:016x}"));
        assert_eq!(drawn.uniform, unit(first));
        assert_eq!(drawn.holdout, unit(first) < 0.1);
        assert_eq!(format!("{hash:016x}"), SESSION_S0_HASH);
        assert_eq!(drawn.seed, SESSION_S0_SEED);
        // 0.3549…: not held out at the default rate.
        assert!(!drawn.holdout);
    }

    const SESSION_S0_HASH: &str = "ec18eac8d758b1eb";
    const SESSION_S0_SEED: &str = "87acb2407b34ac18";

    #[test]
    fn the_rate_holds_out_about_its_share_and_the_extremes_exactly() {
        let held = (0..10_000)
            .filter(|i| draw(&format!("session-{i}"), 0.1).holdout)
            .count();
        assert!((800..=1200).contains(&held), "{held}");
        assert!(!draw("x", 0.0).holdout);
        assert!(draw("x", 1.0).holdout);
    }
}
