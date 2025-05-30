// Copyright (C) Scaleway, S.L. 2025
// Authors:
// *   Thibault Chatel <tchatel@scaleway.com>
// All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::time::{Duration, Instant};

/// Leaky bucket controlling rate limit
pub struct LeakyBucket {
    // Rate at which the bucket leaks, expressed in unit/second
    rate: f64,

    // Maximum value for level before producing a sleep
    size: f64,

    // Bucket level in unit
    level: f64,

    // Timestamp for last access of the bucket
    last_access: Instant,
}

impl LeakyBucket {
    /// Construct a new leaky bucket given a target rate
    pub fn new(rate: u64) -> Self {
        LeakyBucket {
            rate: rate as f64,
            size: rate as f64,
            level: 0 as f64,
            last_access: Instant::now(),
        }
    }

    /// Compute the duration that we should wait before another account
    fn compute_timer(&mut self) -> Duration {
        let now = Instant::now();
        let elapse = now.duration_since(self.last_access).as_secs_f64();

        self.level = 0.0_f64.max(self.level - self.rate * elapse);

        self.last_access = now;
        if self.level > self.size {
            return Duration::from_secs_f64((self.level - self.size) / self.rate);
        }
        Duration::ZERO
    }

    /// Perform accounting
    fn account(&mut self, amount: u64) {
        self.level += amount as f64;
    }

    pub fn consume(&mut self, amount: u64) -> Duration {
        self.account(amount);
        self.compute_timer()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::thread;
    #[test]
    fn test_rate_limiter_ops() {
        // leaky bucket of 1 ops/s
        let mut lb = LeakyBucket::new(1);

        // consume full size without wait
        lb.account(1);
        assert!(lb.compute_timer().as_nanos() == 0);

        // trying to consume more should produce a wait
        lb.account(1);
        assert!(lb.compute_timer().as_nanos() > 0);

        // wait for the bucket to leak
        thread::sleep(Duration::from_secs(2));

        // consume full size without wait
        lb.account(1);
        assert!(lb.compute_timer().as_nanos() == 0);

        // wait for the bucket to leak
        thread::sleep(Duration::from_secs(1));

        // consume full size without wait
        lb.account(1);
        assert!(lb.compute_timer().as_nanos() == 0);
    }

    #[test]
    fn test_rate_limiter_wait() {
        // leaky bucket of 1 ops/s
        let mut lb = LeakyBucket::new(1);

        // consume full size without wait
        lb.account(1);
        assert!(lb.compute_timer().as_nanos() == 0);

        // trying to consume more should produce a wait ~ 1 second
        lb.account(1);
        let timer = lb.compute_timer();
        assert!(timer.as_millis() > 0);
        assert!(timer.as_secs_f64().ceil() == 1.0);
    }
}
