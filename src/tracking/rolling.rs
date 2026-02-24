use std::collections::VecDeque;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapSide {
    Buy,
    Sell,
}

#[derive(Clone, Copy, Debug, Default)]
struct FlowBucket {
    ts_bucket: u64,
    buy_quote_volume: f64,
    sell_quote_volume: f64,
    buy_trades: u64,
    sell_trades: u64,
}

#[derive(Clone, Debug, Default)]
pub struct FlowWindowSummary {
    pub buy_quote_volume: f64,
    pub sell_quote_volume: f64,
    pub buy_trades: u64,
    pub sell_trades: u64,
}

impl FlowWindowSummary {
    pub fn total_trades(&self) -> u64 {
        self.buy_trades + self.sell_trades
    }

    pub fn net_quote_flow(&self) -> f64 {
        self.buy_quote_volume - self.sell_quote_volume
    }

    pub fn buy_sell_ratio(&self) -> Option<f64> {
        if self.sell_quote_volume <= 0.0 {
            if self.buy_quote_volume > 0.0 {
                return Some(f64::INFINITY);
            }
            return None;
        }
        Some(self.buy_quote_volume / self.sell_quote_volume)
    }
}

#[derive(Clone, Debug)]
pub struct RollingFlowStats {
    bucket_secs: u64,
    max_age_secs: u64,
    buckets: VecDeque<FlowBucket>,
    total_recorded_trades: u64,
}

impl RollingFlowStats {
    pub fn new(bucket_secs: u64, max_age_secs: u64) -> Self {
        Self {
            bucket_secs: bucket_secs.max(1),
            max_age_secs: max_age_secs.max(60),
            buckets: VecDeque::new(),
            total_recorded_trades: 0,
        }
    }

    pub fn record(&mut self, ts_unix: u64, side: SwapSide, quote_volume: f64) {
        if !quote_volume.is_finite() || quote_volume <= 0.0 {
            return;
        }
        let bucket_ts = (ts_unix / self.bucket_secs) * self.bucket_secs;
        match self.buckets.back_mut() {
            Some(last) if last.ts_bucket == bucket_ts => apply_to_bucket(last, side, quote_volume),
            _ => {
                let mut bucket = FlowBucket {
                    ts_bucket: bucket_ts,
                    ..FlowBucket::default()
                };
                apply_to_bucket(&mut bucket, side, quote_volume);
                self.buckets.push_back(bucket);
            }
        }
        self.total_recorded_trades = self.total_recorded_trades.saturating_add(1);
        self.evict_old(bucket_ts);
    }

    pub fn summary(&self, now_unix: u64, window_secs: u64) -> FlowWindowSummary {
        if self.buckets.is_empty() {
            return FlowWindowSummary::default();
        }
        let target = now_unix.saturating_sub(window_secs);
        let mut out = FlowWindowSummary::default();
        for bucket in self.buckets.iter().rev() {
            if bucket.ts_bucket < target {
                break;
            }
            out.buy_quote_volume += bucket.buy_quote_volume;
            out.sell_quote_volume += bucket.sell_quote_volume;
            out.buy_trades = out.buy_trades.saturating_add(bucket.buy_trades);
            out.sell_trades = out.sell_trades.saturating_add(bucket.sell_trades);
        }
        out
    }

    pub fn total_recorded_trades(&self) -> u64 {
        self.total_recorded_trades
    }

    fn evict_old(&mut self, latest_bucket_ts: u64) {
        while let Some(front) = self.buckets.front() {
            if latest_bucket_ts.saturating_sub(front.ts_bucket) > self.max_age_secs {
                self.buckets.pop_front();
            } else {
                break;
            }
        }
    }
}

fn apply_to_bucket(bucket: &mut FlowBucket, side: SwapSide, quote_volume: f64) {
    match side {
        SwapSide::Buy => {
            bucket.buy_quote_volume += quote_volume;
            bucket.buy_trades = bucket.buy_trades.saturating_add(1);
        }
        SwapSide::Sell => {
            bucket.sell_quote_volume += quote_volume;
            bucket.sell_trades = bucket.sell_trades.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RollingFlowStats, SwapSide};

    #[test]
    fn rolling_window_counts_and_volumes() {
        let mut s = RollingFlowStats::new(10, 600);
        s.record(100, SwapSide::Buy, 50.0);
        s.record(105, SwapSide::Sell, 20.0);
        s.record(130, SwapSide::Buy, 30.0);
        s.record(380, SwapSide::Sell, 10.0);

        let one_min = s.summary(380, 60);
        assert_eq!(one_min.buy_trades, 0);
        assert_eq!(one_min.sell_trades, 1);
        assert!((one_min.sell_quote_volume - 10.0).abs() < 1e-9);

        let five_min = s.summary(380, 300);
        assert_eq!(five_min.buy_trades, 2);
        assert_eq!(five_min.sell_trades, 2);
        assert!((five_min.buy_quote_volume - 80.0).abs() < 1e-9);
        assert!((five_min.sell_quote_volume - 30.0).abs() < 1e-9);
    }

    #[test]
    fn evicts_old_buckets() {
        let mut s = RollingFlowStats::new(10, 30);
        s.record(100, SwapSide::Buy, 1.0);
        s.record(200, SwapSide::Buy, 1.0);
        let sum = s.summary(200, 300);
        assert_eq!(sum.buy_trades, 1);
        assert_eq!(s.total_recorded_trades(), 2);
    }
}
