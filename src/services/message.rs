use crate::domain::{HolderShare, RugCheckReport, TokenMetadataLite, VenueEvent, symbol_for_mint};
use crate::services::rugcheck::{rug_risk_classification, rug_risk_lines};

fn fmt_opt_usd(v: Option<f64>) -> String {
    match v {
        Some(x) if x >= 1_000_000_000.0 => format!("${:.2} B", x / 1_000_000_000.0),
        Some(x) if x >= 1_000_000.0 => format!("${:.2} M", x / 1_000_000.0),
        Some(x) if x >= 1_000.0 => format!("${:.2} K", x / 1_000.0),
        Some(x) if x >= 1.0 => format!("${x:.4}"),
        Some(x) => format!("${x:.8}"),
        None => "N/A".to_string(),
    }
}

fn fmt_opt_count(v: Option<u64>) -> String {
    match v {
        Some(x) => x.to_string(),
        None => "N/A".to_string(),
    }
}

pub fn build_telegram_message(
    event: &VenueEvent,
    token_meta: Option<&TokenMetadataLite>,
    top_holders: &[HolderShare],
    rugcheck: Option<&RugCheckReport>,
) -> String {
    let token_symbol = token_meta
        .and_then(|m| m.symbol.clone())
        .or_else(|| rugcheck.and_then(|r| r.file_meta.symbol.clone()))
        .or_else(|| rugcheck.and_then(|r| r.token_meta.symbol.clone()))
        .unwrap_or_else(|| symbol_for_mint(&event.token_mint));
    let quote_symbol = symbol_for_mint(&event.quote_mint);
    let token_name = token_meta
        .and_then(|m| m.name.clone())
        .or_else(|| rugcheck.and_then(|r| r.file_meta.name.clone()))
        .or_else(|| rugcheck.and_then(|r| r.token_meta.name.clone()))
        .unwrap_or_else(|| token_symbol.clone());
    let pair_symbol = format!("{token_symbol} / {quote_symbol}");

    let top_holders_line = if top_holders.is_empty() {
        "N/A".to_string()
    } else {
        top_holders
            .iter()
            .map(|h| format!("{:.2}% (https://solscan.io/account/{})", h.pct, h.address))
            .collect::<Vec<_>>()
            .join(" | ")
    };

    let rugcheck_section = if let Some(rc) = rugcheck {
        let classification = rug_risk_classification(rc);
        let risk_score = rc
            .score_normalised
            .map(|v| format!("{v}/100"))
            .unwrap_or_else(|| "N/A".to_string());
        let total_liq = fmt_opt_usd(rc.total_market_liquidity);
        let stable_liq = fmt_opt_usd(rc.total_stable_liquidity);
        let lp_providers = fmt_opt_count(rc.total_lp_providers);
        let total_holders = fmt_opt_count(rc.total_holders);
        let risk_lines = rug_risk_lines(rc, 8);
        let risk_block = if risk_lines.is_empty() {
            "- No major risk flags returned".to_string()
        } else {
            risk_lines.join("\n")
        };

        format!(
            "\n\n🟠 RugCheck information (https://rugcheck.xyz/tokens/{})\n\n\
🧪 Classification: {classification}\n\
📉 Risk Score: {risk_score}\n\
💧 Total Liquidity: {total_liq} (stable: {stable_liq})\n\
🔒 LP Providers: {lp_providers}\n\
👥 Total Holders: {total_holders}\n\n\
{risk_block}",
            event.token_mint
        )
    } else {
        format!(
            "\n\n🟠 RugCheck information (https://rugcheck.xyz/tokens/{})\n\n\
- report unavailable",
            event.token_mint
        )
    };

    format!(
        "New pair detected on {}\n\n\
👀 Token: {token_name} (https://solscan.io/token/{}) ({})\n\
⚖️ Pair: {pair_symbol} (https://solscan.io/account/{})\n\
🆔 Pool ID: {}\n\
👥 Top Holders: {top_holders_line}\n\
🔎 Manual: DexScreener https://dexscreener.com/search?q={} | RugCheck https://rugcheck.xyz/tokens/{}\n\n\
Tx: https://solscan.io/tx/{}{}",
        event.venue.display_name,
        event.token_mint,
        event.token_mint,
        event.pool_id,
        event.pool_id,
        event.token_mint,
        event.token_mint,
        event.signature,
        rugcheck_section,
    )
}
