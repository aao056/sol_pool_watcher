use crate::domain::{HolderShare, VenueEvent};
use crate::tracking::impact::ImpactSnapshot;
use crate::tracking::liquidity::LiquidityHistory;
use crate::tracking::rolling::{FlowWindowSummary, RollingFlowStats};
use std::time::Instant;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoolTrackState {
    New,
    Tracking,
    Quiet,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoolKind {
    Cpmm,
    Clmm,
    Dlmm,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenProgramKind {
    SplToken,
    Token2022,
    Other(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TradeabilityProfile {
    pub mint_authority_active: Option<bool>,
    pub freeze_authority_active: Option<bool>,
    pub token_program: Option<TokenProgramKind>,
    pub token2022_transfer_fee_extension: Option<bool>,
    pub metadata_mutable: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct HolderAggregates {
    pub top1_pct: Option<f64>,
    pub top5_pct: Option<f64>,
    pub top10_pct: Option<f64>,
    pub concentration_score: Option<u8>,
}

impl HolderAggregates {
    pub fn from_holders(holders: &[HolderShare]) -> Self {
        let top1 = holders.first().map(|h| h.pct);
        let top5_sum = sum_pct(holders.iter().take(5).map(|h| h.pct));
        let top10_sum = sum_pct(holders.iter().take(10).map(|h| h.pct));

        // Higher is more concentrated/risky (0 = good distribution, 100 = very concentrated)
        let concentration_score = match (top1, top5_sum, top10_sum) {
            (Some(t1), Some(t5), Some(t10)) => {
                let weighted = (t1 * 0.5) + (t5 * 0.3) + (t10 * 0.2);
                Some(weighted.round().clamp(0.0, 100.0) as u8)
            }
            _ => None,
        };

        Self {
            top1_pct: top1,
            top5_pct: top5_sum,
            top10_pct: top10_sum,
            concentration_score,
        }
    }
}

fn sum_pct<I: Iterator<Item = f64>>(iter: I) -> Option<f64> {
    let mut count = 0usize;
    let mut sum = 0.0;
    for v in iter {
        count += 1;
        sum += v;
    }
    if count == 0 { None } else { Some(sum) }
}

#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub struct RugSnapshot {
    pub classification: Option<String>,
    pub score: Option<u64>,
    pub total_liquidity_usd: Option<f64>,
    pub total_stable_liquidity_usd: Option<f64>,
    pub total_lp_providers: Option<u64>,
    pub total_holders: Option<u64>,
    pub mutable_metadata: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct PoolTrackingSeed {
    pub event: VenueEvent,
    pub detected_at_unix: u64,
    pub token_name: String,
    pub token_symbol: String,
    pub quote_symbol: String,
    pub top_holders: Vec<HolderShare>,
    pub holder_aggregates: HolderAggregates,
    pub rug: RugSnapshot,
}

#[derive(Clone, Debug, Default)]
pub struct SniperScoreSnapshot {
    pub score: i32,
    pub band_emoji: &'static str,
    pub band_label: &'static str,
    pub reasons: Vec<String>,
    pub flags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntrySignalState {
    Go,
    Wait,
    NoGo,
}

#[derive(Clone, Debug)]
pub struct EntrySignalSnapshot {
    pub state: EntrySignalState,
    pub label: &'static str,
    pub emoji: &'static str,
    pub reason: String,
    pub price_display: Option<String>,
}

impl Default for EntrySignalSnapshot {
    fn default() -> Self {
        Self {
            state: EntrySignalState::Wait,
            label: "WAIT",
            emoji: "⏳",
            reason: "insufficient data".to_string(),
            price_display: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ShadowTrade {
    pub opened_at_unix: u64,
    pub entry_mark_ratio: f64,
    pub last_mark_ratio: f64,
    pub best_mark_ratio: f64,
    pub entry_score: i32,
}

#[derive(Clone, Debug, Default)]
pub struct ShadowStats {
    pub closed: u64,
    pub wins: u64,
    pub losses: u64,
    pub cumulative_pnl_pct: f64,
}

#[derive(Clone, Debug)]
pub struct PoolNotificationView {
    pub event: VenueEvent,
    pub token_name: String,
    pub token_symbol: String,
    pub quote_symbol: String,
    pub age_secs: u64,
    pub state: PoolTrackState,
    pub score: SniperScoreSnapshot,
    pub quote_liquidity: Option<f64>,
    pub quote_liquidity_symbol: String,
    pub liquidity_usd: Option<f64>,
    pub delta_quote_60s: Option<f64>,
    pub delta_quote_300s: Option<f64>,
    pub delta_usd_60s: Option<f64>,
    pub delta_usd_300s: Option<f64>,
    pub flow_1m: FlowWindowSummary,
    pub flow_5m: FlowWindowSummary,
    pub flow_seen: bool,
    pub impact: Option<ImpactSnapshot>,
    pub entry_signal: EntrySignalSnapshot,
    pub mark_price_display: Option<String>,
    pub tradeability: TradeabilityProfile,
    pub holder_aggregates: HolderAggregates,
    pub rug: RugSnapshot,
    pub update_reason: Option<String>,
    pub score_delta: Option<i32>,
}

#[derive(Clone, Debug)]
pub struct TrackedPool {
    pub seed: PoolTrackingSeed,
    pub pool_kind: PoolKind,
    pub state: PoolTrackState,
    pub created_at: Instant,
    pub first_seen_unix: u64,
    pub next_refresh_at: Instant,
    pub last_notified_at: Option<Instant>,
    pub last_notified_score: Option<i32>,
    pub tradeability: TradeabilityProfile,
    pub score: SniperScoreSnapshot,
    pub score_break_reasons: Vec<String>,
    pub last_update_reason: Option<String>,
    pub last_updated_at: Instant,
    pub last_top_holders_refresh_at: Option<Instant>,
    pub holders: Vec<HolderShare>,
    pub holder_aggregates: HolderAggregates,
    pub rug: RugSnapshot,
    pub light_phase_until: Instant,
    pub tracking_phase_until: Instant,
    pub liquidity_history: LiquidityHistory,
    pub flow_stats: RollingFlowStats,
    pub impact: Option<ImpactSnapshot>,
    pub entry_signal: EntrySignalSnapshot,
    pub last_entry_alert_state: Option<EntrySignalState>,
    pub last_mark_ratio: Option<f64>,
    pub last_mark_ts_unix: Option<u64>,
    pub shadow_trade: Option<ShadowTrade>,
    pub shadow_trade_completed: bool,
}
