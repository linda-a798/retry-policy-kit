//! Core retry policy types: how long to wait between attempts, and for how long
//! to keep trying before giving up. No I/O lives here; that's the CLI's job.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Backoff {
    Fixed,
    Exponential { multiplier: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Jitter {
    None,
    /// Full jitter: uniform random delay in [0, computed_delay]. See the AWS
    /// architecture blog post on backoff strategies for why this beats a fixed
    /// or partial jitter under contention.
    Full,
}

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub backoff: Backoff,
    pub jitter: Jitter,
}

impl RetryPolicy {
    /// Delay to wait before the given attempt number (1-based: the wait
    /// before attempt 2 is `delay_for_attempt(1, ..)`).
    pub fn delay_for_attempt(&self, attempt: u32, rng_state: &mut u64) -> Duration {
        let base_ms = self.base_delay.as_millis() as f64;
        let raw_ms = match self.backoff {
            Backoff::Fixed => base_ms,
            Backoff::Exponential { multiplier } => base_ms * multiplier.powi(attempt as i32 - 1),
        };
        let capped_ms = raw_ms.min(self.max_delay.as_millis() as f64);
        let final_ms = match self.jitter {
            Jitter::None => capped_ms,
            Jitter::Full => capped_ms * next_random(rng_state),
        };
        Duration::from_millis(final_ms.round() as u64)
    }
}

/// xorshift64*, seeded and stepped by the caller. Good enough for jitter,
/// not for anything security sensitive, and avoids pulling in a rand crate.
fn next_random(state: &mut u64) -> f64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    (x >> 11) as f64 / (1u64 << 53) as f64
}

/// Parses a policy from a small key=value text format, one setting per line.
/// Blank lines and lines starting with '#' are ignored. Recognized keys:
///
/// ```text
/// max_attempts=5
/// base_delay_ms=200
/// max_delay_ms=10000
/// strategy=exponential   # or "fixed"
/// multiplier=2.0         # only used when strategy=exponential
/// jitter=full            # or "none"
/// ```
pub fn parse_policy(text: &str) -> Result<RetryPolicy, String> {
    let mut max_attempts = 5u32;
    let mut base_delay_ms = 200u64;
    let mut max_delay_ms = 10_000u64;
    let mut multiplier = 2.0f64;
    let mut strategy = "exponential".to_string();
    let mut jitter = "none".to_string();

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("line {line_no}: expected key=value, got {raw_line:?}"))?;
        let key = key.trim();
        let value = value.trim();
        match key {
            "max_attempts" => {
                max_attempts = value
                    .parse()
                    .map_err(|_| format!("line {line_no}: invalid max_attempts {value:?}"))?
            }
            "base_delay_ms" => {
                base_delay_ms = value
                    .parse()
                    .map_err(|_| format!("line {line_no}: invalid base_delay_ms {value:?}"))?
            }
            "max_delay_ms" => {
                max_delay_ms = value
                    .parse()
                    .map_err(|_| format!("line {line_no}: invalid max_delay_ms {value:?}"))?
            }
            "multiplier" => {
                multiplier = value
                    .parse()
                    .map_err(|_| format!("line {line_no}: invalid multiplier {value:?}"))?
            }
            "strategy" => strategy = value.to_string(),
            "jitter" => jitter = value.to_string(),
            other => return Err(format!("line {line_no}: unknown key {other:?}")),
        }
    }

    if max_attempts == 0 {
        return Err("max_attempts must be at least 1".to_string());
    }

    let backoff = match strategy.as_str() {
        "fixed" => Backoff::Fixed,
        "exponential" => Backoff::Exponential { multiplier },
        other => return Err(format!("unknown strategy {other:?}, expected fixed or exponential")),
    };

    let jitter = match jitter.as_str() {
        "none" => Jitter::None,
        "full" => Jitter::Full,
        other => return Err(format!("unknown jitter {other:?}, expected none or full")),
    };

    Ok(RetryPolicy {
        max_attempts,
        base_delay: Duration::from_millis(base_delay_ms),
        max_delay: Duration::from_millis(max_delay_ms),
        backoff,
        jitter,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_backoff_ignores_attempt_number() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            backoff: Backoff::Fixed,
            jitter: Jitter::None,
        };
        let mut state = 42;
        assert_eq!(policy.delay_for_attempt(1, &mut state), Duration::from_millis(100));
        assert_eq!(policy.delay_for_attempt(5, &mut state), Duration::from_millis(100));
    }

    #[test]
    fn exponential_backoff_is_capped() {
        let policy = RetryPolicy {
            max_attempts: 10,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
            backoff: Backoff::Exponential { multiplier: 2.0 },
            jitter: Jitter::None,
        };
        let mut state = 42;
        assert_eq!(policy.delay_for_attempt(1, &mut state), Duration::from_millis(100));
        assert_eq!(policy.delay_for_attempt(2, &mut state), Duration::from_millis(200));
        assert_eq!(policy.delay_for_attempt(3, &mut state), Duration::from_millis(400));
        assert_eq!(policy.delay_for_attempt(4, &mut state), Duration::from_millis(500));
    }

    #[test]
    fn full_jitter_stays_within_bounds() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1000),
            max_delay: Duration::from_secs(10),
            backoff: Backoff::Fixed,
            jitter: Jitter::Full,
        };
        let mut state = 12345;
        for _ in 0..100 {
            let delay = policy.delay_for_attempt(1, &mut state);
            assert!(delay <= Duration::from_millis(1000));
        }
    }

    #[test]
    fn parse_policy_rejects_unknown_key() {
        let err = parse_policy("bogus=1").unwrap_err();
        assert!(err.contains("unknown key"));
    }
}
