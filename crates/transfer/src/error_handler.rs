use iris_core_domain::types::error::BackendError;
use std::time::Duration;

/// Exponential backoff with a retry limit.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub backoff_factor: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            backoff_factor: 2.0,
        }
    }
}

impl RetryPolicy {
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let delay_secs =
            self.initial_delay.as_secs_f64() * self.backoff_factor.powi(attempt as i32);
        let delay = Duration::from_secs_f64(delay_secs);
        if delay > self.max_delay {
            self.max_delay
        } else {
            delay
        }
    }

    pub fn should_retry(&self, attempt: u32, error: &BackendError) -> bool {
        if attempt >= self.max_retries {
            return false;
        }

        match error {
            BackendError::Network(_) => true,
            // An expired token can succeed after a refresh.
            BackendError::Auth(detail) if detail.is_token_expired => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exponential_backoff() {
        let policy = RetryPolicy::default();
        let d0 = policy.delay_for_attempt(0);
        let d1 = policy.delay_for_attempt(1);
        let d2 = policy.delay_for_attempt(2);

        assert_eq!(d0.as_secs(), 1);
        assert_eq!(d1.as_secs(), 2);
        assert_eq!(d2.as_secs(), 4);
    }

    #[test]
    fn test_max_delay_cap() {
        let policy = RetryPolicy {
            max_delay: Duration::from_secs(10),
            ..RetryPolicy::default()
        };

        let d10 = policy.delay_for_attempt(10);
        assert!(d10 <= Duration::from_secs(10));
    }
}
