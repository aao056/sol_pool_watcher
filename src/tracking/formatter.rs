use crate::tracking::impact::ImpactSnapshot;
use crate::tracking::model::{
    EntrySignalState, PoolNotificationView, PoolTrackState, TokenProgramKind, TradeabilityProfile,
};

fn fmt_usd_short(v: Option<f64>) -> String {
    match v {
        Some(x) if x >= 1_000_000_000.0 => format!("${:.2}B", x / 1_000_000_000.0),
        Some(x) if x >= 1_000_000.0 => format!("${:.2}M", x / 1_000_000.0),
        Some(x) if x >= 1_000.0 => format!("${:.2}K", x / 1_000.0),
        Some(x) if x >= 1.0 => format!("${x:.2}"),
        Some(x) if x > 0.0 => format!("${x:.6}"),
        Some(_) => "$0".to_string(),
        None => "N/A".to_string(),
    }
}

fn fmt_qty(v: Option<f64>, symbol: &str) -> String {
    match v {
        Some(x) if x >= 1000.0 => format!("{:.2}K {symbol}", x / 1000.0),
        Some(x) => format!("{x:.4} {symbol}"),
        None => "N/A".to_string(),
    }
}

fn fmt_delta(v: Option<f64>, symbol: &str) -> String {
    match v {
        Some(x) if x > 0.0 => format!("+{}", fmt_qty(Some(x), symbol)),
        Some(x) => format!("-{}", fmt_qty(Some(x.abs()), symbol)),
        None => "N/A".to_string(),
    }
}

fn fmt_delta_usd(v: Option<f64>) -> String {
    match v {
        Some(x) if x > 0.0 => format!("+{}", fmt_usd_short(Some(x))),
        Some(x) => format!("-{}", fmt_usd_short(Some(x.abs()))),
        None => "N/A".to_string(),
    }
}

fn fmt_pct(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1}%"))
        .unwrap_or_else(|| "N/A".to_string())
}

fn fmt_bps_pct(v: Option<u64>, approx: bool) -> String {
    match v {
        Some(bps) => {
            let prefix = if approx { "~" } else { "" };
            format!("{prefix}{:.2}%", bps as f64 / 100.0)
        }
        None => "N/A".to_string(),
    }
}

fn is_stable_quote(symbol: &str) -> bool {
    matches!(symbol, "USDC" | "USDT")
}

fn fmt_flow_amount(quote_symbol: &str, amount_quote: f64) -> String {
    if is_stable_quote(quote_symbol) {
        if amount_quote.abs() >= 1000.0 {
            format!(
                "{}${:.2}K",
                if amount_quote >= 0.0 { "+" } else { "-" },
                amount_quote.abs() / 1000.0
            )
        } else {
            format!(
                "{}${:.2}",
                if amount_quote >= 0.0 { "+" } else { "-" },
                amount_quote.abs()
            )
        }
    } else {
        format!(
            "{}{:.4} {}",
            if amount_quote >= 0.0 { "+" } else { "-" },
            amount_quote.abs(),
            quote_symbol
        )
    }
}

fn flow_line(view: &PoolNotificationView) -> String {
    if !view.flow_seen {
        if flow_tracking_wired_for_venue(view) {
            return "Flow 1m/5m: 0 / 0 (waiting for first swap)".to_string();
        }
        return "Flow 1m/5m: pending (swap tracking not wired yet for this venue)".to_string();
    }
    format!(
        "Flow 1m/5m: {} / {}  (T {}/{}, B {}/{}, S {}/{})",
        fmt_flow_amount(&view.quote_symbol, view.flow_1m.net_quote_flow()),
        fmt_flow_amount(&view.quote_symbol, view.flow_5m.net_quote_flow()),
        view.flow_1m.total_trades(),
        view.flow_5m.total_trades(),
        view.flow_1m.buy_trades,
        view.flow_5m.buy_trades,
        view.flow_1m.sell_trades,
        view.flow_5m.sell_trades,
    )
}

fn flow_tracking_wired_for_venue(view: &PoolNotificationView) -> bool {
    matches!(
        (
            view.event.venue.dex.as_str(),
            view.event.venue.kind.as_str()
        ),
        ("meteora", "damm")
    )
}

fn impact_line(impact: Option<ImpactSnapshot>) -> String {
    let Some(impact) = impact else {
        return "Impact: N/A".to_string();
    };
    let approx = impact.approx;
    let size_small = if impact.quote_is_usd {
        impact
            .quote_size_small
            .map(|v| format!("${v:.0}"))
            .unwrap_or_else(|| "small".to_string())
    } else {
        impact
            .quote_size_small
            .map(|v| format!("{v:.3}"))
            .unwrap_or_else(|| "small".to_string())
    };
    let size_large = if impact.quote_is_usd {
        impact
            .quote_size_large
            .map(|v| format!("${v:.0}"))
            .unwrap_or_else(|| "large".to_string())
    } else {
        impact
            .quote_size_large
            .map(|v| format!("{v:.3}"))
            .unwrap_or_else(|| "large".to_string())
    };

    format!(
        "Impact: {} B {} | S {}  •  {} B {} | S {}",
        size_small,
        fmt_bps_pct(impact.buy_small_bps, approx),
        fmt_bps_pct(impact.sell_small_bps, approx),
        size_large,
        fmt_bps_pct(impact.buy_large_bps, approx),
        fmt_bps_pct(impact.sell_large_bps, approx),
    )
}

fn entry_line(view: &PoolNotificationView) -> String {
    let price = view
        .mark_price_display
        .clone()
        .or_else(|| view.entry_signal.price_display.clone())
        .unwrap_or_else(|| "N/A".to_string());
    format!(
        "Entry: {} {} | Price: {} | {}",
        view.entry_signal.emoji, view.entry_signal.label, price, view.entry_signal.reason
    )
}

fn liquidity_row_label(view: &PoolNotificationView) -> &'static str {
    if view.quote_liquidity.is_some() {
        "💧 Pool Lq:"
    } else if view.liquidity_usd.is_some() {
        "💧 Lq (token-wide est.):"
    } else {
        "💧 Lq:"
    }
}

fn tradeability_line(t: &TradeabilityProfile) -> String {
    let mint = match t.mint_authority_active {
        Some(false) => "mint:renounced",
        Some(true) => "mint:active",
        None => "mint:?",
    };
    let freeze = match t.freeze_authority_active {
        Some(false) => "freeze:none",
        Some(true) => "freeze:active",
        None => "freeze:?",
    };
    let program = match &t.token_program {
        Some(TokenProgramKind::SplToken) => "SPL",
        Some(TokenProgramKind::Token2022) => "T22",
        Some(TokenProgramKind::Other(_)) => "other",
        None => "?",
    };
    let t22_fee = match t.token2022_transfer_fee_extension {
        Some(true) => " +fee-ext",
        Some(false) => "",
        None => "",
    };
    let meta = match t.metadata_mutable {
        Some(true) => " | meta:mutable",
        Some(false) => " | meta:immutable",
        None => "",
    };

    format!("{mint} | {freeze} | {program}{t22_fee}{meta}")
}

fn flags_line(view: &PoolNotificationView) -> String {
    if view.score.flags.is_empty() {
        return "Flags: N/A".to_string();
    }
    format!("Flags: {}", view.score.flags.join(" | "))
}

fn manual_links_line(view: &PoolNotificationView) -> String {
    format!(
        "Manual: DexScreener https://dexscreener.com/search?q={} | RugCheck https://rugcheck.xyz/tokens/{}",
        view.event.token_mint, view.event.token_mint
    )
}

pub fn format_new_pool_message(view: &PoolNotificationView) -> String {
    let liq_line = match (view.quote_liquidity, view.liquidity_usd) {
        (Some(q), Some(usd)) => format!(
            "{} (~{})",
            fmt_qty(Some(q), &view.quote_liquidity_symbol),
            fmt_usd_short(Some(usd))
        ),
        (Some(q), None) => fmt_qty(Some(q), &view.quote_liquidity_symbol),
        (None, Some(usd)) => format!("~{}", fmt_usd_short(Some(usd))),
        (None, None) => "N/A".to_string(),
    };

    let state_label = match view.state {
        PoolTrackState::New => "NEW",
        PoolTrackState::Tracking => "TRACKING",
        PoolTrackState::Quiet => "QUIET",
    };

    let rug_line = match (&view.rug.classification, view.rug.score) {
        (Some(c), Some(s)) => format!("{c} ({s}/100)"),
        (Some(c), None) => c.clone(),
        (None, Some(s)) => format!("N/A ({s}/100)"),
        (None, None) => "N/A".to_string(),
    };

    format!(
        "🆕 New Pool • {}\n\n👀 Token: {} ({}) https://solscan.io/token/{}\n⚖️ Pair: {}/{} https://solscan.io/account/{}\n🧠 SniperScore: {}/100 {} ({})  |  RC: {}\n⏱️ Age: {}s  |  State: {}\n{} {}\n📉 Liquidity Δ: 1m {} | 5m {}\n📊 {}\n🎯 {}\n🚀 {}\n🧪 Tradeability: {}\n👥 Holders: Top1 {} | Top5 {} | Top10 {} | Conc {}/100\n🚦 {}\n🔎 {}\n🆔 Pool: {}\n🧾 Tx: https://solscan.io/tx/{}",
        view.event.venue.display_name,
        view.token_name,
        view.token_symbol,
        view.event.token_mint,
        view.token_symbol,
        view.quote_symbol,
        view.event.pool_id,
        view.score.score,
        view.score.band_emoji,
        view.score.band_label,
        rug_line,
        view.age_secs,
        state_label,
        liquidity_row_label(view),
        liq_line,
        if view.delta_quote_60s.is_some() {
            fmt_delta(view.delta_quote_60s, &view.quote_liquidity_symbol)
        } else {
            fmt_delta_usd(view.delta_usd_60s)
        },
        if view.delta_quote_300s.is_some() {
            fmt_delta(view.delta_quote_300s, &view.quote_liquidity_symbol)
        } else {
            fmt_delta_usd(view.delta_usd_300s)
        },
        flow_line(view),
        impact_line(view.impact),
        entry_line(view),
        tradeability_line(&view.tradeability),
        fmt_pct(view.holder_aggregates.top1_pct),
        fmt_pct(view.holder_aggregates.top5_pct),
        fmt_pct(view.holder_aggregates.top10_pct),
        view.holder_aggregates
            .concentration_score
            .map(|v| v.to_string())
            .unwrap_or_else(|| "N/A".to_string()),
        flags_line(view),
        manual_links_line(view),
        view.event.pool_id,
        view.event.signature,
    )
}

pub fn format_update_pool_message(view: &PoolNotificationView) -> String {
    let liq_line = match (view.quote_liquidity, view.liquidity_usd) {
        (Some(q), Some(usd)) => format!(
            "{} (~{})",
            fmt_qty(Some(q), &view.quote_liquidity_symbol),
            fmt_usd_short(Some(usd))
        ),
        (Some(q), None) => fmt_qty(Some(q), &view.quote_liquidity_symbol),
        (None, Some(usd)) => format!("~{}", fmt_usd_short(Some(usd))),
        (None, None) => "N/A".to_string(),
    };

    let score_delta = view.score_delta.unwrap_or(0);
    let delta_fmt = if score_delta >= 0 {
        format!("+{score_delta}")
    } else {
        score_delta.to_string()
    };

    format!(
        "🔄 Pool Update • {} • {}/{}\n\n⚖️ Pair: {}/{} https://solscan.io/account/{}\n🧠 SniperScore: {}/100 {} ({})\n⏱️ Age: {}s  |  {} {}\n📉 Liquidity Δ: 1m {} | 5m {}\n📊 {}\n🎯 {}\n🚀 {}\n🚦 {}\n🔎 {}\n📝 Reason: {}\n🆔 Pool: {}",
        view.event.venue.display_name,
        view.token_symbol,
        view.quote_symbol,
        view.token_symbol,
        view.quote_symbol,
        view.event.pool_id,
        view.score.score,
        view.score.band_emoji,
        delta_fmt,
        view.age_secs,
        liquidity_row_label(view),
        liq_line,
        if view.delta_quote_60s.is_some() {
            fmt_delta(view.delta_quote_60s, &view.quote_liquidity_symbol)
        } else {
            fmt_delta_usd(view.delta_usd_60s)
        },
        if view.delta_quote_300s.is_some() {
            fmt_delta(view.delta_quote_300s, &view.quote_liquidity_symbol)
        } else {
            fmt_delta_usd(view.delta_usd_300s)
        },
        flow_line(view),
        impact_line(view.impact),
        entry_line(view),
        flags_line(view),
        manual_links_line(view),
        view.update_reason
            .clone()
            .unwrap_or_else(|| "state change".to_string()),
        view.event.pool_id,
    )
}

pub fn format_entry_go_message(view: &PoolNotificationView) -> String {
    if view.entry_signal.state != EntrySignalState::Go {
        return format_update_pool_message(view);
    }

    let price = view
        .mark_price_display
        .clone()
        .or_else(|| view.entry_signal.price_display.clone())
        .unwrap_or_else(|| "N/A".to_string());
    let state_label = match view.state {
        PoolTrackState::New => "NEW",
        PoolTrackState::Tracking => "TRACKING",
        PoolTrackState::Quiet => "QUIET",
    };
    let rug_line = match (&view.rug.classification, view.rug.score) {
        (Some(c), Some(s)) => format!("{c} ({s}/100)"),
        (Some(c), None) => c.clone(),
        (None, Some(s)) => format!("N/A ({s}/100)"),
        (None, None) => "N/A".to_string(),
    };

    format!(
        "🚀 GO Entry Signal • {}\n\n👀 Token: {} ({}) https://solscan.io/token/{}\n⚖️ Pair: {}/{} https://solscan.io/account/{}\n🧠 SniperScore: {}/100 {} ({}) | RC: {}\n💵 Entry Price: {}\n⏱️ Age: {}s | State: {}\n📊 {}\n🚦 {}\n📝 Rule: {}\n🔎 {}\n🆔 Pool: {}\n🧾 Tx: https://solscan.io/tx/{}",
        view.event.venue.display_name,
        view.token_name,
        view.token_symbol,
        view.event.token_mint,
        view.token_symbol,
        view.quote_symbol,
        view.event.pool_id,
        view.score.score,
        view.score.band_emoji,
        view.score.band_label,
        rug_line,
        price,
        view.age_secs,
        state_label,
        flow_line(view),
        flags_line(view),
        view.entry_signal.reason,
        manual_links_line(view),
        view.event.pool_id,
        view.event.signature,
    )
}

#[cfg(test)]
mod tests {
    use super::format_new_pool_message;
    use crate::domain::{VenueEvent, VenueId};
    use crate::tracking::model::{
        EntrySignalSnapshot, EntrySignalState, HolderAggregates, PoolNotificationView,
        PoolTrackState, RugSnapshot, SniperScoreSnapshot, TradeabilityProfile,
    };

    #[test]
    fn formats_new_message_snapshot() {
        let view = PoolNotificationView {
            event: VenueEvent {
                venue: VenueId::new("raydium", "clmm", "Raydium CLMM"),
                signature: "sig".to_string(),
                pool_id: "pool".to_string(),
                token_mint: "token".to_string(),
                quote_mint: "quote".to_string(),
            },
            token_name: "Token".to_string(),
            token_symbol: "TKN".to_string(),
            quote_symbol: "SOL".to_string(),
            age_secs: 42,
            state: PoolTrackState::New,
            score: SniperScoreSnapshot {
                score: 71,
                band_emoji: "🟢",
                band_label: "Go",
                reasons: vec![],
                flags: vec![
                    "✅ Mint renounced".to_string(),
                    "✅ Freeze none".to_string(),
                ],
            },
            quote_liquidity: Some(12.3),
            quote_liquidity_symbol: "SOL".to_string(),
            liquidity_usd: Some(24500.0),
            delta_quote_60s: Some(1.0),
            delta_quote_300s: None,
            delta_usd_60s: None,
            delta_usd_300s: Some(1500.0),
            flow_1m: Default::default(),
            flow_5m: Default::default(),
            flow_seen: false,
            impact: None,
            entry_signal: EntrySignalSnapshot {
                state: EntrySignalState::Wait,
                label: "WAIT",
                emoji: "⏳",
                reason: "waiting first swap".to_string(),
                price_display: None,
            },
            mark_price_display: None,
            tradeability: TradeabilityProfile::default(),
            holder_aggregates: HolderAggregates {
                top1_pct: Some(4.0),
                top5_pct: Some(15.0),
                top10_pct: Some(25.0),
                concentration_score: Some(14),
            },
            rug: RugSnapshot {
                classification: Some("Low Risk".to_string()),
                score: Some(5),
                ..Default::default()
            },
            update_reason: None,
            score_delta: None,
        };

        let msg = format_new_pool_message(&view);
        assert!(msg.contains("SniperScore: 71/100 🟢"));
        assert!(msg.contains("Liquidity Δ:"));
        assert!(msg.contains("Flow 1m/5m:"));
        assert!(msg.contains("Impact:"));
        assert!(msg.contains("Flags:"));
        assert!(msg.contains("dexscreener.com/search?q=token"));
        assert!(msg.contains("rugcheck.xyz/tokens/token"));
    }
}
