use crate::config::TrackingConfig;
use crate::domain::{HolderShare, RugCheckReport};
use crate::notifier::TelegramNotifier;
use crate::services::analytics::fetch_top_holders;
use crate::services::rugcheck::{
    fetch_rugcheck_report, holders_from_rugcheck, rug_risk_classification,
};
use crate::tracking::formatter::{
    format_entry_go_message, format_new_pool_message, format_update_pool_message,
};
use crate::tracking::impact::{ImpactSnapshot, estimate_cpmm_impact_snapshot};
use crate::tracking::liquidity::{LiquidityHistory, LiquiditySample};
use crate::tracking::model::{
    EntrySignalSnapshot, EntrySignalState, HolderAggregates, PoolKind, PoolNotificationView,
    PoolTrackState, PoolTrackingSeed, RugSnapshot, ShadowStats, ShadowTrade, SniperScoreSnapshot,
    TrackedPool, TradeabilityProfile,
};
use crate::tracking::rolling::{RollingFlowStats, SwapSide};
use crate::tracking::score::{SniperScoreInputs, compute_sniper_score};
use crate::tracking::tradeability::fetch_tradeability_profile;
use reqwest::Client as HttpClient;
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::time::timeout;
use tracing::{debug, info};

pub struct TrackingManager {
    cfg: TrackingConfig,
    http_client: HttpClient,
    rpc_url: String,
    telegram: Option<TelegramNotifier>,
    pools: HashMap<String, TrackedPool>,
    shadow_stats: ShadowStats,
}

impl TrackingManager {
    pub fn new(
        cfg: TrackingConfig,
        http_client: HttpClient,
        rpc_url: String,
        telegram: Option<TelegramNotifier>,
    ) -> Option<Self> {
        if !cfg.enabled {
            return None;
        }
        Some(Self {
            cfg,
            http_client,
            rpc_url,
            telegram,
            pools: HashMap::new(),
            shadow_stats: ShadowStats::default(),
        })
    }

    pub fn poll_interval_secs(&self) -> u64 {
        self.cfg.tick_interval_secs.max(1)
    }

    pub async fn on_new_pool(&mut self, seed: PoolTrackingSeed) {
        let key = seed.event.dedupe_key();
        if self.pools.contains_key(&key) {
            return;
        }

        let now = Instant::now();
        let mut tradeability = self
            .fetch_tradeability_with_timeout(&seed.event.token_mint)
            .await;
        if tradeability.metadata_mutable.is_none() {
            tradeability.metadata_mutable = seed.rug.mutable_metadata;
        }

        let mut pool = TrackedPool {
            seed: seed.clone(),
            pool_kind: pool_kind_for_event(&seed.event.venue.dex, &seed.event.venue.kind),
            state: PoolTrackState::New,
            created_at: now,
            first_seen_unix: seed.detected_at_unix,
            next_refresh_at: now + Duration::from_secs(self.cfg.refresh_interval_secs.max(1)),
            last_notified_at: Some(now),
            last_notified_score: None,
            tradeability,
            score: SniperScoreSnapshot::default(),
            score_break_reasons: Vec::new(),
            last_update_reason: None,
            last_updated_at: now,
            last_top_holders_refresh_at: Some(now),
            holders: seed.top_holders.clone(),
            holder_aggregates: seed.holder_aggregates.clone(),
            rug: seed.rug.clone(),
            light_phase_until: now + Duration::from_secs(self.cfg.light_tracking_secs.max(1)),
            tracking_phase_until: now
                + Duration::from_secs(
                    self.cfg.light_tracking_secs.max(1) + self.cfg.tracking_secs.max(1),
                ),
            liquidity_history: LiquidityHistory::new(self.cfg.liquidity_history_secs.max(300)),
            flow_stats: RollingFlowStats::new(5, 5 * 60 + 30),
            impact: None,
            entry_signal: EntrySignalSnapshot::default(),
            last_entry_alert_state: None,
            last_mark_ratio: None,
            last_mark_ts_unix: None,
            shadow_trade: None,
            shadow_trade_completed: false,
        };

        pool.liquidity_history.push(LiquiditySample {
            ts_unix: now_unix_secs(),
            quote_liquidity: fallback_quote_liquidity_from_rug(
                &pool.seed.quote_symbol,
                pool.rug.total_liquidity_usd,
            ),
            usd_liquidity: pool.rug.total_liquidity_usd,
        });
        pool.impact = self.estimate_impact(&pool);
        Self::recompute_score(&mut pool);
        pool.entry_signal = Self::compute_entry_signal(&self.cfg, &pool);
        pool.last_notified_score = Some(pool.score.score);

        let view = self.build_view(&pool, None, None);
        info!(
            venue = %pool.seed.event.venue.slug(),
            pool_id = %pool.seed.event.pool_id,
            sniper_score = pool.score.score,
            state = "new",
            "tracking pool created"
        );
        self.notify_default(&format_new_pool_message(&view)).await;

        self.pools.insert(key, pool);
        self.evict_if_needed();
    }

    pub async fn on_tick(&mut self) {
        if self.pools.is_empty() {
            return;
        }

        let now = Instant::now();
        let keys = self.pools.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            let Some(mut pool) = self.pools.remove(&key) else {
                continue;
            };

            let keep = self.process_pool(&mut pool, now).await;
            if keep {
                self.pools.insert(key, pool);
            }
        }
    }

    #[allow(dead_code)]
    pub fn on_swap_quote_flow(
        &mut self,
        pool_dedupe_key: &str,
        side: SwapSide,
        quote_volume: f64,
        quote_liquidity: Option<f64>,
        mark_price_ratio: Option<f64>,
        ts_unix: u64,
    ) {
        let cfg = self.cfg.clone();
        if let Some(pool) = self.pools.get_mut(pool_dedupe_key) {
            let prev_entry_state = pool.entry_signal.state.clone();
            pool.flow_stats.record(ts_unix, side, quote_volume);
            if quote_liquidity.is_some() {
                pool.liquidity_history.push(LiquiditySample {
                    ts_unix,
                    quote_liquidity,
                    usd_liquidity: pool
                        .liquidity_history
                        .latest()
                        .and_then(|s| s.usd_liquidity)
                        .or(pool.rug.total_liquidity_usd),
                });
            }
            if let Some(mark) = mark_price_ratio.filter(|v| v.is_finite() && *v > 0.0) {
                pool.last_mark_ratio = Some(mark);
                pool.last_mark_ts_unix = Some(ts_unix);
            }
            Self::recompute_score(pool);
            // Entry signal is derived from score/state/liquidity/flow and may change on swaps.
            pool.entry_signal = Self::compute_entry_signal(&cfg, pool);
            if pool.entry_signal.state != EntrySignalState::Go
                && pool.entry_signal.state != prev_entry_state
            {
                pool.last_entry_alert_state = Some(pool.entry_signal.state.clone());
            }
        }
    }

    async fn process_pool(&mut self, pool: &mut TrackedPool, now: Instant) -> bool {
        if pool.state == PoolTrackState::Quiet {
            let age = now.duration_since(pool.created_at).as_secs();
            return age
                < self
                    .cfg
                    .evict_after_secs
                    .max(self.cfg.tracking_secs + self.cfg.light_tracking_secs + 60);
        }

        if now >= pool.tracking_phase_until {
            if pool.state != PoolTrackState::Quiet {
                pool.state = PoolTrackState::Quiet;
                info!(
                    venue = %pool.seed.event.venue.slug(),
                    pool_id = %pool.seed.event.pool_id,
                    "tracking state changed to quiet (time window elapsed)"
                );
            }
            return true;
        }

        if pool.state == PoolTrackState::New && now >= pool.light_phase_until {
            if self.should_promote(pool) {
                pool.state = PoolTrackState::Tracking;
                info!(
                    venue = %pool.seed.event.venue.slug(),
                    pool_id = %pool.seed.event.pool_id,
                    sniper_score = pool.score.score,
                    "tracking state changed to TRACKING"
                );
                self.maybe_send_update(pool, "promoted_to_tracking", Some(0), true)
                    .await;
                self.maybe_send_entry_go_alert(pool).await;
            } else {
                pool.state = PoolTrackState::Quiet;
                let promotion_failures = self.promotion_failure_reasons(pool);
                info!(
                    venue = %pool.seed.event.venue.slug(),
                    pool_id = %pool.seed.event.pool_id,
                    sniper_score = pool.score.score,
                    failures = ?promotion_failures,
                    "tracking state changed to QUIET (did not pass promotion gate)"
                );
                return true;
            }
        }

        if now < pool.next_refresh_at {
            self.maybe_send_entry_go_alert(pool).await;
            self.maybe_process_shadow_trade(pool).await;
            return true;
        }

        let prev_score = pool.score.score;
        let prev_tradeability = pool.tradeability.clone();
        let before_drop_ratio_30 = pool.liquidity_history.delta_usd_ratio_secs(30);
        let refresh_reason = self.refresh_pool(pool).await;
        let after_drop_ratio_30 = pool
            .liquidity_history
            .delta_usd_ratio_secs(30)
            .or(before_drop_ratio_30);
        let score_delta = pool.score.score - prev_score;
        let authority_changed = authority_changed(&prev_tradeability, &pool.tradeability);
        let liq_drop_trigger =
            after_drop_ratio_30.is_some_and(|r| r <= -self.cfg.liquidity_drop_alert_ratio);
        let prev_entry_signal = pool.entry_signal.state.clone();
        pool.entry_signal = Self::compute_entry_signal(&self.cfg, pool);

        if score_delta.abs() >= self.cfg.update_score_delta_threshold as i32 {
            self.maybe_send_update(pool, "score_change", Some(score_delta), false)
                .await;
        } else if liq_drop_trigger {
            self.maybe_send_update(pool, "liquidity_drop", Some(score_delta), true)
                .await;
        } else if authority_changed {
            self.maybe_send_update(pool, "authority_change", Some(score_delta), true)
                .await;
        } else if let Some(reason) = refresh_reason {
            debug!(
                venue = %pool.seed.event.venue.slug(),
                pool_id = %pool.seed.event.pool_id,
                reason = %reason,
                "tracking refresh completed"
            );
        }

        if prev_entry_signal != pool.entry_signal.state {
            self.maybe_send_entry_go_alert(pool).await;
            pool.last_entry_alert_state = Some(pool.entry_signal.state.clone());
        }

        self.maybe_process_shadow_trade(pool).await;

        pool.next_refresh_at =
            Instant::now() + Duration::from_secs(self.cfg.refresh_interval_secs.max(1));
        true
    }

    async fn refresh_pool(&self, pool: &mut TrackedPool) -> Option<String> {
        let mut reason = None;

        if let Some(rc) = self
            .fetch_rugcheck_with_timeout(&pool.seed.event.token_mint)
            .await
        {
            pool.rug = rug_snapshot_from_report(&rc);
            if pool.tradeability.metadata_mutable.is_none() {
                pool.tradeability.metadata_mutable = pool.rug.mutable_metadata;
            }
            let holders = holders_from_rugcheck(&rc, 10);
            if !holders.is_empty() {
                pool.holders = holders;
                pool.holder_aggregates = HolderAggregates::from_holders(&pool.holders);
                pool.last_top_holders_refresh_at = Some(Instant::now());
            }
            reason = Some("rugcheck_refresh".to_string());
        } else {
            let should_refresh_holders = pool
                .last_top_holders_refresh_at
                .map(|t| {
                    t.elapsed()
                        >= Duration::from_secs(self.cfg.holders_refresh_interval_secs.max(30))
                })
                .unwrap_or(true);
            if should_refresh_holders
                && let Some(holders) = self
                    .fetch_top_holders_with_timeout(&pool.seed.event.token_mint)
                    .await
                && !holders.is_empty()
            {
                pool.holders = holders;
                pool.holder_aggregates = HolderAggregates::from_holders(&pool.holders);
                pool.last_top_holders_refresh_at = Some(Instant::now());
                reason = Some("holders_refresh".to_string());
            }
        }

        let tradeability = self
            .fetch_tradeability_with_timeout(&pool.seed.event.token_mint)
            .await;
        if tradeability != TradeabilityProfile::default() {
            let mut merged = tradeability;
            if merged.metadata_mutable.is_none() {
                merged.metadata_mutable = pool.rug.mutable_metadata;
            }
            pool.tradeability = merged;
            if reason.is_none() {
                reason = Some("tradeability_refresh".to_string());
            }
        }

        pool.liquidity_history.push(LiquiditySample {
            ts_unix: now_unix_secs(),
            quote_liquidity: fallback_quote_liquidity_from_rug(
                &pool.seed.quote_symbol,
                pool.rug.total_liquidity_usd,
            ),
            usd_liquidity: pool.rug.total_liquidity_usd,
        });
        pool.last_updated_at = Instant::now();
        pool.impact = self.estimate_impact(pool);
        Self::recompute_score(pool);
        pool.entry_signal = Self::compute_entry_signal(&self.cfg, pool);

        reason
    }

    fn recompute_score(pool: &mut TrackedPool) {
        let now = now_unix_secs();
        let flow_1m = pool.flow_stats.summary(now, 60);
        let impact_small_bps = pool
            .impact
            .and_then(|i| i.buy_small_bps.zip(i.sell_small_bps))
            .map(|(b, s)| b.max(s));
        let inputs = SniperScoreInputs {
            liquidity_usd: pool.rug.total_liquidity_usd,
            delta_liq_60_ratio: pool.liquidity_history.delta_usd_ratio_secs(60),
            delta_liq_300_ratio: pool.liquidity_history.delta_usd_ratio_secs(300),
            quote_liquidity: pool
                .liquidity_history
                .latest()
                .and_then(|s| s.quote_liquidity),
            net_flow_1m_quote: Some(flow_1m.net_quote_flow()),
            buy_sell_ratio_1m: flow_1m.buy_sell_ratio(),
            trades_1m: Some(flow_1m.total_trades()),
            impact_small_bps,
            impact_is_approx: pool.impact.map(|i| i.approx),
            holders: pool.holder_aggregates.clone(),
            tradeability: pool.tradeability.clone(),
            rug: pool.rug.clone(),
        };
        let score = compute_sniper_score(&inputs);
        pool.score_break_reasons = score.reasons.clone();
        pool.score = score;
    }

    fn should_promote(&self, pool: &TrackedPool) -> bool {
        let liq_ok = pool
            .rug
            .total_liquidity_usd
            .map(|v| v >= self.cfg.promote_min_liquidity_usd)
            .unwrap_or(false);
        let risk_ok = if self.cfg.promote_require_low_risk {
            pool.rug.classification.as_deref() == Some("Low Risk")
        } else {
            true
        };
        let authorities_ok = pool.tradeability.mint_authority_active != Some(true)
            && pool.tradeability.freeze_authority_active != Some(true);
        let score_ok = pool.score.score >= self.cfg.promote_min_score as i32;
        score_ok && risk_ok && authorities_ok && liq_ok
    }

    fn promotion_failure_reasons(&self, pool: &TrackedPool) -> Vec<String> {
        let mut out = Vec::new();

        if pool.score.score < self.cfg.promote_min_score as i32 {
            out.push(format!(
                "score {} < min {}",
                pool.score.score, self.cfg.promote_min_score
            ));
        }

        match pool.rug.total_liquidity_usd {
            Some(liq) if liq >= self.cfg.promote_min_liquidity_usd => {}
            Some(liq) => out.push(format!(
                "liq ${:.2} < min ${:.2} (RugCheck token-wide est.)",
                liq, self.cfg.promote_min_liquidity_usd
            )),
            None => out.push("liq unknown".to_string()),
        }

        if self.cfg.promote_require_low_risk
            && pool.rug.classification.as_deref() != Some("Low Risk")
        {
            out.push(format!(
                "rugcheck classification = {}",
                pool.rug
                    .classification
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string())
            ));
        }

        if pool.tradeability.mint_authority_active == Some(true) {
            out.push("mint authority active".to_string());
        }
        if pool.tradeability.freeze_authority_active == Some(true) {
            out.push("freeze authority active".to_string());
        }

        if out.is_empty() {
            out.push("unknown gate failure".to_string());
        }
        out
    }

    async fn maybe_send_update(
        &mut self,
        pool: &mut TrackedPool,
        reason: &str,
        score_delta: Option<i32>,
        bypass_cooldown: bool,
    ) {
        let now = Instant::now();
        if !bypass_cooldown
            && pool.last_notified_at.is_some_and(|t| {
                now.duration_since(t) < Duration::from_secs(self.cfg.notify_cooldown_secs.max(1))
            })
        {
            return;
        }

        let view = self.build_view(pool, Some(reason.to_string()), score_delta);
        self.notify_default(&format_update_pool_message(&view))
            .await;
        pool.last_notified_at = Some(now);
        pool.last_notified_score = Some(pool.score.score);
        pool.last_update_reason = Some(reason.to_string());
    }

    async fn maybe_send_entry_go_alert(&mut self, pool: &mut TrackedPool) {
        if pool.entry_signal.state != EntrySignalState::Go {
            return;
        }
        if pool.last_entry_alert_state == Some(EntrySignalState::Go) {
            return;
        }
        let view = self.build_view(pool, Some("entry_go".to_string()), Some(0));
        self.notify_sim(&format_entry_go_message(&view)).await;
        pool.last_entry_alert_state = Some(EntrySignalState::Go);
        pool.last_notified_at = Some(Instant::now());
    }

    fn build_view(
        &self,
        pool: &TrackedPool,
        update_reason: Option<String>,
        score_delta: Option<i32>,
    ) -> PoolNotificationView {
        let latest = pool.liquidity_history.latest();
        let now = now_unix_secs();
        let flow_1m = pool.flow_stats.summary(now, 60);
        let flow_5m = pool.flow_stats.summary(now, 300);
        PoolNotificationView {
            event: pool.seed.event.clone(),
            token_name: pool.seed.token_name.clone(),
            token_symbol: pool.seed.token_symbol.clone(),
            quote_symbol: pool.seed.quote_symbol.clone(),
            age_secs: now_unix_secs().saturating_sub(pool.first_seen_unix),
            state: pool.state.clone(),
            score: pool.score.clone(),
            quote_liquidity: latest.and_then(|s| s.quote_liquidity),
            quote_liquidity_symbol: pool.seed.quote_symbol.clone(),
            liquidity_usd: latest
                .and_then(|s| s.usd_liquidity)
                .or(pool.rug.total_liquidity_usd),
            delta_quote_60s: pool.liquidity_history.delta_quote_secs(60),
            delta_quote_300s: pool.liquidity_history.delta_quote_secs(300),
            delta_usd_60s: pool.liquidity_history.delta_usd_secs(60),
            delta_usd_300s: pool.liquidity_history.delta_usd_secs(300),
            flow_1m,
            flow_5m,
            flow_seen: pool.flow_stats.total_recorded_trades() > 0,
            impact: pool.impact,
            entry_signal: pool.entry_signal.clone(),
            mark_price_display: None,
            tradeability: pool.tradeability.clone(),
            holder_aggregates: pool.holder_aggregates.clone(),
            rug: pool.rug.clone(),
            update_reason,
            score_delta,
        }
    }

    async fn notify_default(&self, text: &str) {
        if let Some(tg) = &self.telegram {
            tg.send_event(text).await;
        }
    }

    async fn notify_sim(&self, text: &str) {
        if let Some(tg) = &self.telegram {
            tg.send_sim(text).await;
        }
    }

    async fn maybe_process_shadow_trade(&mut self, pool: &mut TrackedPool) {
        if !self.cfg.shadow_enabled {
            return;
        }
        if pool.pool_kind != PoolKind::Cpmm {
            return;
        }
        if pool.shadow_trade_completed {
            return;
        }

        let now_unix = now_unix_secs();

        if pool.shadow_trade.is_none() {
            if pool.entry_signal.state == EntrySignalState::Go {
                let Some(mark) = pool.last_mark_ratio.filter(|m| m.is_finite() && *m > 0.0) else {
                    return;
                };
                let trade = ShadowTrade {
                    opened_at_unix: now_unix,
                    entry_mark_ratio: mark,
                    last_mark_ratio: mark,
                    best_mark_ratio: mark,
                    entry_score: pool.score.score,
                };
                pool.shadow_trade = Some(trade);
                let msg = self.format_shadow_open_message(pool);
                self.notify_sim(&msg).await;
            }
            return;
        }

        let Some(mark) = pool.last_mark_ratio.filter(|m| m.is_finite() && *m > 0.0) else {
            return;
        };

        let mut close_reason: Option<&'static str> = None;
        let mut close_pnl_pct = 0.0f64;
        let mut hold_secs = 0u64;
        let mut entry_score = 0i32;
        let mut best_pnl_pct = 0.0f64;

        if let Some(trade) = pool.shadow_trade.as_mut() {
            trade.last_mark_ratio = mark;
            if mark > trade.best_mark_ratio {
                trade.best_mark_ratio = mark;
            }

            let pnl_pct = ((mark / trade.entry_mark_ratio) - 1.0) * 100.0;
            let tp_pct = self.cfg.shadow_take_profit_bps as f64 / 100.0;
            let sl_pct = self.cfg.shadow_stop_loss_bps as f64 / 100.0;
            let max_hold = self.cfg.shadow_max_hold_secs;
            let elapsed = now_unix.saturating_sub(trade.opened_at_unix);

            if pnl_pct >= tp_pct {
                close_reason = Some("take_profit");
            } else if pnl_pct <= -sl_pct {
                close_reason = Some("stop_loss");
            } else if elapsed >= max_hold {
                close_reason = Some("time_stop");
            }

            if close_reason.is_some() {
                close_pnl_pct = pnl_pct;
                hold_secs = elapsed;
                entry_score = trade.entry_score;
                best_pnl_pct = ((trade.best_mark_ratio / trade.entry_mark_ratio) - 1.0) * 100.0;
            }
        }

        if let Some(reason) = close_reason {
            pool.shadow_trade = None;
            pool.shadow_trade_completed = true;
            self.record_shadow_close(close_pnl_pct);
            let msg = self.format_shadow_close_message(
                pool,
                reason,
                close_pnl_pct,
                best_pnl_pct,
                hold_secs,
                entry_score,
            );
            self.notify_sim(&msg).await;
        }
    }

    fn record_shadow_close(&mut self, pnl_pct: f64) {
        self.shadow_stats.closed += 1;
        self.shadow_stats.cumulative_pnl_pct += pnl_pct;
        if pnl_pct >= 0.0 {
            self.shadow_stats.wins += 1;
        } else {
            self.shadow_stats.losses += 1;
        }
    }

    fn format_shadow_open_message(&self, pool: &TrackedPool) -> String {
        let Some(trade) = pool.shadow_trade.as_ref() else {
            return String::new();
        };
        format!(
            "🧪 Shadow BUY opened • {}\n\n👀 Token: {} ({}) https://solscan.io/token/{}\n⚖️ Pair: {}/{} https://solscan.io/account/{}\n🧠 Entry Signal: {} {} | Score {} | RC {}\n💵 Entry Price: N/A (on-chain mark index tracked)\n⏱️ Opened: {}s after detection\n🆔 Pool: {}\n🧾 Tx: https://solscan.io/tx/{}",
            pool.seed.event.venue.display_name,
            pool.seed.token_name,
            pool.seed.token_symbol,
            pool.seed.event.token_mint,
            pool.seed.token_symbol,
            pool.seed.quote_symbol,
            pool.seed.event.pool_id,
            pool.entry_signal.emoji,
            pool.entry_signal.label,
            trade.entry_score,
            pool.rug
                .classification
                .clone()
                .unwrap_or_else(|| "N/A".to_string()),
            trade.opened_at_unix.saturating_sub(pool.first_seen_unix),
            pool.seed.event.pool_id,
            pool.seed.event.signature,
        )
    }

    fn format_shadow_close_message(
        &self,
        pool: &TrackedPool,
        reason: &str,
        pnl_pct: f64,
        best_pnl_pct: f64,
        hold_secs: u64,
        entry_score: i32,
    ) -> String {
        let closed = self.shadow_stats.closed.max(1);
        let win_rate = (self.shadow_stats.wins as f64 / closed as f64) * 100.0;
        let avg_pnl = self.shadow_stats.cumulative_pnl_pct / closed as f64;
        format!(
            "🧪 Shadow EXIT • {}\n\n👀 Token: {} ({}) https://solscan.io/token/{}\n⚖️ Pair: {}/{} https://solscan.io/account/{}\n📉 PnL: {:+.2}%  |  Peak: {:+.2}%\n🧠 Entry Score: {} | Exit Reason: {}\n⏱️ Hold: {}s\n📊 Shadow Stats: closed {} | wins {} | losses {} | win rate {:.1}% | avg {:+.2}%\n🆔 Pool: {}",
            pool.seed.event.venue.display_name,
            pool.seed.token_name,
            pool.seed.token_symbol,
            pool.seed.event.token_mint,
            pool.seed.token_symbol,
            pool.seed.quote_symbol,
            pool.seed.event.pool_id,
            pnl_pct,
            best_pnl_pct,
            entry_score,
            reason,
            hold_secs,
            self.shadow_stats.closed,
            self.shadow_stats.wins,
            self.shadow_stats.losses,
            win_rate,
            avg_pnl,
            pool.seed.event.pool_id,
        )
    }

    async fn fetch_rugcheck_with_timeout(&self, token_mint: &str) -> Option<RugCheckReport> {
        timeout(
            Duration::from_millis(self.cfg.request_timeout_ms.max(100)),
            fetch_rugcheck_report(&self.http_client, token_mint),
        )
        .await
        .ok()
        .flatten()
    }

    async fn fetch_tradeability_with_timeout(&self, token_mint: &str) -> TradeabilityProfile {
        timeout(
            Duration::from_millis(self.cfg.request_timeout_ms.max(100)),
            fetch_tradeability_profile(&self.http_client, &self.rpc_url, token_mint),
        )
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
    }

    async fn fetch_top_holders_with_timeout(&self, token_mint: &str) -> Option<Vec<HolderShare>> {
        timeout(
            Duration::from_millis(self.cfg.request_timeout_ms.max(100)),
            fetch_top_holders(&self.http_client, &self.rpc_url, token_mint, 10),
        )
        .await
        .ok()
        .flatten()
    }

    fn evict_if_needed(&mut self) {
        let cap = self.cfg.max_tracked_pools.max(10) as usize;
        while self.pools.len() > cap {
            let candidate = self
                .pools
                .iter()
                .filter(|(_, p)| p.state == PoolTrackState::Quiet)
                .min_by_key(|(_, p)| p.created_at)
                .map(|(k, _)| k.clone())
                .or_else(|| {
                    self.pools
                        .iter()
                        .min_by_key(|(_, p)| p.created_at)
                        .map(|(k, _)| k.clone())
                });
            let Some(key) = candidate else {
                break;
            };
            self.pools.remove(&key);
        }
    }

    fn estimate_impact(&self, pool: &TrackedPool) -> Option<ImpactSnapshot> {
        if pool.pool_kind != PoolKind::Cpmm {
            return None;
        }
        let total_liq_usd = pool.rug.total_liquidity_usd?;
        if total_liq_usd <= 0.0 {
            return None;
        }

        // Phase 2A fallback: for stable-quoted CPMM pools, approximate a balanced pool
        // from total USD liquidity until venue-specific reserve snapshots are wired.
        let quote_is_usd = matches!(pool.seed.quote_symbol.as_str(), "USDC" | "USDT");
        if !quote_is_usd {
            return None;
        }

        let quote_reserve = total_liq_usd / 2.0;
        let base_reserve = quote_reserve; // normalized price assumption for approximate impact
        Some(estimate_cpmm_impact_snapshot(
            base_reserve,
            quote_reserve,
            self.cfg.cpmm_assumed_fee_bps,
            self.cfg.impact_small_usd,
            self.cfg.impact_large_usd,
            true,
            true,
        ))
    }

    fn compute_entry_signal(cfg: &TrackingConfig, pool: &TrackedPool) -> EntrySignalSnapshot {
        let mut reasons = Vec::new();
        let score_ok = pool.score.score >= 70;
        if !score_ok {
            reasons.push(format!("score {} < 70", pool.score.score));
        }

        let state_ok = pool.state == PoolTrackState::Tracking;
        if !state_ok {
            reasons.push("not promoted".to_string());
        }

        let risk_ok = pool.rug.classification.as_deref() == Some("Low Risk");
        if !risk_ok {
            reasons.push(format!(
                "RC {}",
                pool.rug
                    .classification
                    .clone()
                    .unwrap_or_else(|| "N/A".to_string())
            ));
        }

        let liq_ok = pool
            .rug
            .total_liquidity_usd
            .is_some_and(|v| v >= cfg.promote_min_liquidity_usd);
        if !liq_ok {
            reasons.push(format!("liq < ${:.0}", cfg.promote_min_liquidity_usd));
        }

        let authorities_ok = pool.tradeability.mint_authority_active != Some(true)
            && pool.tradeability.freeze_authority_active != Some(true);
        if !authorities_ok {
            reasons.push("authorities".to_string());
        }

        let top1_ok = pool.holder_aggregates.top1_pct.is_some_and(|v| v <= 20.0);
        if !top1_ok {
            reasons.push("top1 > 20%".to_string());
        }

        let now = now_unix_secs();
        let flow_1m = pool.flow_stats.summary(now, 60);
        let first_swap_seen =
            flow_1m.total_trades() > 0 || pool.flow_stats.total_recorded_trades() > 0;
        if !first_swap_seen {
            reasons.push("waiting first swap".to_string());
        }
        let flow_ok = !first_swap_seen || flow_1m.net_quote_flow() > 0.0;
        if first_swap_seen && !flow_ok {
            reasons.push("net flow <= 0".to_string());
        }

        let price_display = None;

        if score_ok
            && state_ok
            && risk_ok
            && liq_ok
            && authorities_ok
            && top1_ok
            && first_swap_seen
            && flow_ok
        {
            return EntrySignalSnapshot {
                state: EntrySignalState::Go,
                label: "GO",
                emoji: "🟢",
                reason: "rule set passed".to_string(),
                price_display,
            };
        }

        let no_go = pool.score.score < 45
            || pool.tradeability.mint_authority_active == Some(true)
            || pool.tradeability.freeze_authority_active == Some(true)
            || pool.rug.classification.as_deref() == Some("High Risk")
            || pool.rug.classification.as_deref() == Some("Likely Rug")
            || pool.holder_aggregates.top1_pct.is_some_and(|v| v > 35.0);

        EntrySignalSnapshot {
            state: if no_go {
                EntrySignalState::NoGo
            } else {
                EntrySignalState::Wait
            },
            label: if no_go { "NO-GO" } else { "WAIT" },
            emoji: if no_go { "🔴" } else { "⏳" },
            reason: reasons.into_iter().take(3).collect::<Vec<_>>().join(" | "),
            price_display,
        }
    }
}

fn authority_changed(prev: &TradeabilityProfile, next: &TradeabilityProfile) -> bool {
    prev.mint_authority_active != next.mint_authority_active
        || prev.freeze_authority_active != next.freeze_authority_active
        || prev.token2022_transfer_fee_extension != next.token2022_transfer_fee_extension
}

fn pool_kind_for_event(dex: &str, kind: &str) -> PoolKind {
    match (dex, kind) {
        ("pumpswap", "amm") => PoolKind::Cpmm,
        ("meteora", "damm") => PoolKind::Cpmm,
        ("raydium", "amm") => PoolKind::Cpmm,
        ("raydium", "clmm") => PoolKind::Clmm,
        ("orca", "whirlpool") => PoolKind::Clmm,
        ("meteora", "dlmm") => PoolKind::Dlmm,
        _ => PoolKind::Unknown,
    }
}

fn fallback_quote_liquidity_from_rug(
    quote_symbol: &str,
    total_liquidity_usd: Option<f64>,
) -> Option<f64> {
    let liq = total_liquidity_usd?;
    if !liq.is_finite() || liq <= 0.0 {
        return None;
    }
    match quote_symbol {
        "USDC" | "USDT" => Some(liq),
        _ => None,
    }
}

pub fn rug_snapshot_from_report(report: &RugCheckReport) -> RugSnapshot {
    RugSnapshot {
        classification: Some(rug_risk_classification(report).to_string()),
        score: report.score_normalised,
        total_liquidity_usd: report.total_market_liquidity,
        total_stable_liquidity_usd: report.total_stable_liquidity,
        total_lp_providers: report.total_lp_providers,
        total_holders: report.total_holders,
        mutable_metadata: report.token_meta.mutable,
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
