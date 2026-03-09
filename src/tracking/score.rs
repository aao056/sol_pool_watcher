use crate::tracking::model::{
    HolderAggregates, RugSnapshot, SniperScoreSnapshot, TradeabilityProfile,
};

#[derive(Clone, Debug, Default)]
pub struct SniperScoreInputs {
    pub liquidity_usd: Option<f64>,
    pub delta_liq_60_ratio: Option<f64>,
    pub delta_liq_300_ratio: Option<f64>,
    pub quote_liquidity: Option<f64>,
    pub net_flow_1m_quote: Option<f64>,
    pub buy_sell_ratio_1m: Option<f64>,
    pub trades_1m: Option<u64>,
    pub impact_small_bps: Option<u64>,
    pub impact_is_approx: Option<bool>,
    pub holders: HolderAggregates,
    pub tradeability: TradeabilityProfile,
    pub rug: RugSnapshot,
}

pub fn compute_sniper_score(inputs: &SniperScoreInputs) -> SniperScoreSnapshot {
    let mut score = 50i32;
    let mut reasons = Vec::new();
    let mut flags = Vec::new();

    if let Some(liq) = inputs.liquidity_usd {
        match liq {
            x if x < 2_000.0 => {
                score -= 20;
                reasons.push("very low liquidity".to_string());
            }
            x if x < 10_000.0 => {
                score -= 10;
                reasons.push("low liquidity".to_string());
            }
            x if x < 25_000.0 => {}
            x if x < 100_000.0 => {
                score += 10;
                reasons.push("decent liquidity".to_string());
            }
            _ => {
                score += 15;
                reasons.push("strong liquidity".to_string());
            }
        }
    } else {
        score -= 5;
        reasons.push("liquidity unknown".to_string());
    }

    if let Some(r) = inputs.delta_liq_60_ratio {
        if r > 0.20 {
            score += 8;
            reasons.push("liq +20% (1m)".to_string());
        } else if r > 0.05 {
            score += 4;
            reasons.push("liq growing (1m)".to_string());
        } else if r < -0.30 {
            score -= 15;
            reasons.push("liq -30% (1m)".to_string());
        } else if r < -0.15 {
            score -= 8;
            reasons.push("liq dropping (1m)".to_string());
        }
    }

    if let Some(r) = inputs.delta_liq_300_ratio {
        if r > 0.30 {
            score += 5;
            reasons.push("liq +30% (5m)".to_string());
        } else if r < -0.25 {
            score -= 8;
            reasons.push("liq -25% (5m)".to_string());
        }
    }

    if let (Some(net_flow), Some(liq_quote)) = (inputs.net_flow_1m_quote, inputs.quote_liquidity)
        && liq_quote > 0.0
    {
        let ratio = net_flow / liq_quote;
        if ratio > 0.10 {
            score += 10;
            reasons.push("strong net buy flow (1m)".to_string());
        } else if ratio > 0.03 {
            score += 5;
            reasons.push("positive net flow (1m)".to_string());
        } else if ratio < -0.10 {
            score -= 12;
            reasons.push("strong negative flow (1m)".to_string());
        } else if ratio < -0.05 {
            score -= 8;
            reasons.push("negative net flow (1m)".to_string());
        }
    }

    if let Some(ratio) = inputs.buy_sell_ratio_1m {
        if ratio.is_finite() {
            if ratio >= 2.0 {
                score += 5;
            } else if ratio >= 1.2 {
                score += 2;
            } else if ratio < 0.5 {
                score -= 8;
            } else if ratio < 0.8 {
                score -= 4;
            }
        } else if ratio.is_infinite() {
            score += 5;
        }
    }

    if let Some(trades) = inputs.trades_1m {
        if trades >= 10 {
            score += 3;
        } else if trades > 0 && trades < 2 {
            score -= 2;
        }
    }

    if let Some(top1) = inputs.holders.top1_pct {
        if top1 <= 5.0 {
            score += 8;
        } else if top1 <= 10.0 {
            score += 4;
        } else if top1 > 50.0 {
            score -= 25;
            reasons.push("top1 > 50%".to_string());
        } else if top1 > 35.0 {
            score -= 15;
            reasons.push("top1 > 35%".to_string());
        } else if top1 > 20.0 {
            score -= 8;
            reasons.push("top1 > 20%".to_string());
        }
        flags.push(if top1 > 20.0 {
            format!("⚠️ Top1 {:.1}%", top1)
        } else {
            format!("✅ Top1 {:.1}%", top1)
        });
    } else {
        flags.push("⚠️ Holders pending".to_string());
        score -= 4;
    }

    if let Some(top10) = inputs.holders.top10_pct {
        if top10 <= 45.0 {
            score += 4;
        } else if top10 > 90.0 {
            score -= 12;
            reasons.push("top10 > 90%".to_string());
        } else if top10 > 75.0 {
            score -= 6;
            reasons.push("top10 > 75%".to_string());
        }
    }

    match inputs.tradeability.mint_authority_active {
        Some(false) => {
            score += 5;
            flags.push("✅ Mint renounced".to_string());
        }
        Some(true) => {
            score -= 20;
            reasons.push("mint authority active".to_string());
            flags.push("⚠️ Mint authority active".to_string());
        }
        None => {
            score -= 3;
            flags.push("⚠️ Mint auth unknown".to_string());
        }
    }

    match inputs.tradeability.freeze_authority_active {
        Some(false) => {
            score += 5;
            flags.push("✅ Freeze none".to_string());
        }
        Some(true) => {
            score -= 15;
            reasons.push("freeze authority active".to_string());
            flags.push("⚠️ Freeze active".to_string());
        }
        None => {
            score -= 2;
            flags.push("⚠️ Freeze unknown".to_string());
        }
    }

    if let Some(has_fee) = inputs.tradeability.token2022_transfer_fee_extension
        && has_fee
    {
        score -= 8;
        reasons.push("token2022 transfer fee".to_string());
        flags.push("⚠️ Transfer fee ext".to_string());
    }

    if let Some(is_mutable) = inputs.tradeability.metadata_mutable
        && is_mutable
    {
        score -= 3;
        reasons.push("mutable metadata".to_string());
    }

    if let Some(impact_bps) = inputs.impact_small_bps {
        let mut weight_scale = 1.0;
        if inputs.impact_is_approx == Some(true) {
            weight_scale = 0.5;
        }
        let impact_adj = if impact_bps > 1500 {
            -10
        } else if impact_bps > 800 {
            -5
        } else if impact_bps < 200 {
            5
        } else {
            0
        };
        score += ((impact_adj as f64) * weight_scale).round() as i32;
        if impact_adj < 0 {
            reasons.push(format!("high impact (~{}bp)", impact_bps));
        }
    }

    match inputs.rug.classification.as_deref() {
        Some("Low Risk") => score += 4,
        Some("Medium Risk") => {
            score -= 6;
            reasons.push("rugcheck medium risk".to_string());
        }
        Some("High Risk") => {
            score -= 15;
            reasons.push("rugcheck high risk".to_string());
        }
        Some("Likely Rug") => {
            score -= 30;
            reasons.push("rugcheck likely rug".to_string());
        }
        Some(_) => {}
        None => {
            score -= 3;
            reasons.push("rugcheck missing".to_string());
        }
    }

    if let Some(rc_score) = inputs.rug.score {
        if rc_score <= 10 {
            score += 4;
        } else if rc_score > 60 {
            score -= 10;
        } else if rc_score > 30 {
            score -= 4;
        }
    }

    let score = score.clamp(0, 100);
    let (band_emoji, band_label) = if score >= 70 {
        ("🟢", "Go")
    } else if score >= 45 {
        ("🟠", "Watch")
    } else {
        ("🔴", "No-Go")
    };

    flags.dedup();
    if flags.len() > 4 {
        flags.truncate(4);
    }

    SniperScoreSnapshot {
        score,
        band_emoji,
        band_label,
        reasons,
        flags,
    }
}

#[cfg(test)]
mod tests {
    use super::{SniperScoreInputs, compute_sniper_score};
    use crate::tracking::model::{
        HolderAggregates, RugSnapshot, TokenProgramKind, TradeabilityProfile,
    };

    #[test]
    fn score_rewards_good_profile() {
        let inputs = SniperScoreInputs {
            liquidity_usd: Some(120_000.0),
            delta_liq_60_ratio: Some(0.25),
            delta_liq_300_ratio: Some(0.35),
            holders: HolderAggregates {
                top1_pct: Some(4.0),
                top5_pct: Some(18.0),
                top10_pct: Some(30.0),
                concentration_score: Some(15),
            },
            tradeability: TradeabilityProfile {
                mint_authority_active: Some(false),
                freeze_authority_active: Some(false),
                token_program: Some(TokenProgramKind::SplToken),
                token2022_transfer_fee_extension: Some(false),
                metadata_mutable: Some(false),
            },
            rug: RugSnapshot {
                classification: Some("Low Risk".to_string()),
                score: Some(5),
                ..Default::default()
            },
            quote_liquidity: Some(120_000.0),
            net_flow_1m_quote: Some(20_000.0),
            buy_sell_ratio_1m: Some(2.5),
            trades_1m: Some(12),
            impact_small_bps: Some(120),
            impact_is_approx: Some(false),
        };
        let out = compute_sniper_score(&inputs);
        assert!(out.score >= 70, "{}", out.score);
        assert_eq!(out.band_emoji, "🟢");
    }

    #[test]
    fn score_penalizes_risky_profile() {
        let inputs = SniperScoreInputs {
            liquidity_usd: Some(500.0),
            delta_liq_60_ratio: Some(-0.4),
            delta_liq_300_ratio: Some(-0.3),
            holders: HolderAggregates {
                top1_pct: Some(80.0),
                top5_pct: Some(98.0),
                top10_pct: Some(99.0),
                concentration_score: Some(95),
            },
            tradeability: TradeabilityProfile {
                mint_authority_active: Some(true),
                freeze_authority_active: Some(true),
                token_program: Some(TokenProgramKind::Token2022),
                token2022_transfer_fee_extension: Some(true),
                metadata_mutable: Some(true),
            },
            rug: RugSnapshot {
                classification: Some("High Risk".to_string()),
                score: Some(80),
                ..Default::default()
            },
            quote_liquidity: Some(500.0),
            net_flow_1m_quote: Some(-200.0),
            buy_sell_ratio_1m: Some(0.4),
            trades_1m: Some(1),
            impact_small_bps: Some(1800),
            impact_is_approx: Some(false),
        };
        let out = compute_sniper_score(&inputs);
        assert!(out.score < 45, "{}", out.score);
        assert_eq!(out.band_emoji, "🔴");
    }
}
