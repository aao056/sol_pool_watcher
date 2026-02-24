use std::collections::VecDeque;

#[derive(Clone, Debug, Default)]
pub struct LiquiditySample {
    pub ts_unix: u64,
    pub quote_liquidity: Option<f64>,
    pub usd_liquidity: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct LiquidityHistory {
    samples: VecDeque<LiquiditySample>,
    max_age_secs: u64,
}

impl LiquidityHistory {
    pub fn new(max_age_secs: u64) -> Self {
        Self {
            samples: VecDeque::new(),
            max_age_secs: max_age_secs.max(60),
        }
    }

    pub fn push(&mut self, sample: LiquiditySample) {
        self.samples.push_back(sample);
        self.evict_old();
    }

    pub fn latest(&self) -> Option<&LiquiditySample> {
        self.samples.back()
    }

    pub fn delta_quote_secs(&self, seconds: u64) -> Option<f64> {
        let latest = self.samples.back()?;
        let prior = self.find_prior(seconds, latest.ts_unix)?;
        Some(latest.quote_liquidity? - prior.quote_liquidity?)
    }

    pub fn delta_usd_secs(&self, seconds: u64) -> Option<f64> {
        let latest = self.samples.back()?;
        let prior = self.find_prior(seconds, latest.ts_unix)?;
        Some(latest.usd_liquidity? - prior.usd_liquidity?)
    }

    pub fn delta_usd_ratio_secs(&self, seconds: u64) -> Option<f64> {
        let latest = self.samples.back()?;
        let prior = self.find_prior(seconds, latest.ts_unix)?;
        let prev = prior.usd_liquidity?;
        if prev <= 0.0 {
            return None;
        }
        Some((latest.usd_liquidity? - prev) / prev)
    }

    fn find_prior(&self, seconds: u64, latest_ts: u64) -> Option<&LiquiditySample> {
        let target = latest_ts.saturating_sub(seconds);
        self.samples.iter().rev().find(|s| s.ts_unix <= target)
    }

    fn evict_old(&mut self) {
        let latest_ts = self.samples.back().map(|s| s.ts_unix).unwrap_or(0);
        while let Some(front) = self.samples.front() {
            if latest_ts.saturating_sub(front.ts_unix) > self.max_age_secs {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LiquidityHistory, LiquiditySample};

    #[test]
    fn computes_liquidity_deltas() {
        let mut hist = LiquidityHistory::new(600);
        hist.push(LiquiditySample {
            ts_unix: 100,
            quote_liquidity: Some(10.0),
            usd_liquidity: Some(1000.0),
        });
        hist.push(LiquiditySample {
            ts_unix: 160,
            quote_liquidity: Some(13.0),
            usd_liquidity: Some(1300.0),
        });
        hist.push(LiquiditySample {
            ts_unix: 400,
            quote_liquidity: Some(11.0),
            usd_liquidity: Some(1100.0),
        });

        assert_eq!(hist.delta_quote_secs(60), Some(-2.0)); // target 340 -> compare to ts=160
        assert_eq!(hist.delta_quote_secs(300), Some(1.0)); // target 100 -> 11 - 10
        assert_eq!(hist.delta_usd_secs(300), Some(100.0));
        let ratio = hist.delta_usd_ratio_secs(300).unwrap();
        assert!((ratio - 0.1).abs() < 1e-9);
    }

    #[test]
    fn evicts_old_samples() {
        let mut hist = LiquidityHistory::new(100);
        hist.push(LiquiditySample {
            ts_unix: 10,
            quote_liquidity: Some(1.0),
            usd_liquidity: Some(1.0),
        });
        hist.push(LiquiditySample {
            ts_unix: 200,
            quote_liquidity: Some(2.0),
            usd_liquidity: Some(2.0),
        });
        assert!(hist.delta_quote_secs(300).is_none());
        assert_eq!(hist.latest().and_then(|s| s.quote_liquidity), Some(2.0));
    }
}
