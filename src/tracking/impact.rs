#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ImpactSnapshot {
    pub approx: bool,
    pub quote_size_small: Option<f64>,
    pub quote_size_large: Option<f64>,
    pub quote_is_usd: bool,
    pub buy_small_bps: Option<u64>,
    pub sell_small_bps: Option<u64>,
    pub buy_large_bps: Option<u64>,
    pub sell_large_bps: Option<u64>,
}

pub fn estimate_cpmm_buy_impact_bps(
    base_reserve: f64,
    quote_reserve: f64,
    quote_in: f64,
    fee_bps: u64,
) -> Option<u64> {
    if !base_reserve.is_finite() || !quote_reserve.is_finite() || !quote_in.is_finite() {
        return None;
    }
    if base_reserve <= 0.0 || quote_reserve <= 0.0 || quote_in <= 0.0 {
        return None;
    }

    let fee_factor = 1.0 - (fee_bps as f64 / 10_000.0);
    if fee_factor <= 0.0 {
        return None;
    }

    let k = base_reserve * quote_reserve;
    let quote_in_eff = quote_in * fee_factor;
    let new_quote = quote_reserve + quote_in_eff;
    if new_quote <= 0.0 {
        return None;
    }
    let new_base = k / new_quote;
    let base_out = base_reserve - new_base;
    if base_out <= 0.0 {
        return None;
    }

    let spot_price = quote_reserve / base_reserve;
    let effective_price = quote_in / base_out;
    if !spot_price.is_finite() || spot_price <= 0.0 || !effective_price.is_finite() {
        return None;
    }

    Some((((effective_price / spot_price) - 1.0).max(0.0) * 10_000.0).round() as u64)
}

pub fn estimate_cpmm_sell_impact_bps_from_quote_notional(
    base_reserve: f64,
    quote_reserve: f64,
    quote_notional_at_spot: f64,
    fee_bps: u64,
) -> Option<u64> {
    if !base_reserve.is_finite()
        || !quote_reserve.is_finite()
        || !quote_notional_at_spot.is_finite()
    {
        return None;
    }
    if base_reserve <= 0.0 || quote_reserve <= 0.0 || quote_notional_at_spot <= 0.0 {
        return None;
    }

    let spot_price = quote_reserve / base_reserve;
    if spot_price <= 0.0 || !spot_price.is_finite() {
        return None;
    }
    let base_in = quote_notional_at_spot / spot_price;
    if base_in <= 0.0 || !base_in.is_finite() {
        return None;
    }

    let fee_factor = 1.0 - (fee_bps as f64 / 10_000.0);
    if fee_factor <= 0.0 {
        return None;
    }

    let base_in_eff = base_in * fee_factor;
    let k = base_reserve * quote_reserve;
    let new_base = base_reserve + base_in_eff;
    if new_base <= 0.0 {
        return None;
    }
    let new_quote = k / new_base;
    let quote_out = quote_reserve - new_quote;
    if quote_out <= 0.0 {
        return None;
    }

    let effective_price = quote_out / base_in;
    let impact = (1.0 - (effective_price / spot_price)).max(0.0);
    Some((impact * 10_000.0).round() as u64)
}

pub fn estimate_cpmm_impact_snapshot(
    base_reserve: f64,
    quote_reserve: f64,
    fee_bps: u64,
    small_quote_size: f64,
    large_quote_size: f64,
    approx: bool,
    quote_is_usd: bool,
) -> ImpactSnapshot {
    ImpactSnapshot {
        approx,
        quote_size_small: Some(small_quote_size),
        quote_size_large: Some(large_quote_size),
        quote_is_usd,
        buy_small_bps: estimate_cpmm_buy_impact_bps(
            base_reserve,
            quote_reserve,
            small_quote_size,
            fee_bps,
        ),
        sell_small_bps: estimate_cpmm_sell_impact_bps_from_quote_notional(
            base_reserve,
            quote_reserve,
            small_quote_size,
            fee_bps,
        ),
        buy_large_bps: estimate_cpmm_buy_impact_bps(
            base_reserve,
            quote_reserve,
            large_quote_size,
            fee_bps,
        ),
        sell_large_bps: estimate_cpmm_sell_impact_bps_from_quote_notional(
            base_reserve,
            quote_reserve,
            large_quote_size,
            fee_bps,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        estimate_cpmm_buy_impact_bps, estimate_cpmm_impact_snapshot,
        estimate_cpmm_sell_impact_bps_from_quote_notional,
    };

    #[test]
    fn cpmm_buy_impact_increases_with_size() {
        let small = estimate_cpmm_buy_impact_bps(10_000.0, 10_000.0, 100.0, 30).unwrap();
        let large = estimate_cpmm_buy_impact_bps(10_000.0, 10_000.0, 1000.0, 30).unwrap();
        assert!(large > small);
    }

    #[test]
    fn cpmm_sell_impact_increases_with_size() {
        let small =
            estimate_cpmm_sell_impact_bps_from_quote_notional(10_000.0, 10_000.0, 100.0, 30)
                .unwrap();
        let large =
            estimate_cpmm_sell_impact_bps_from_quote_notional(10_000.0, 10_000.0, 1000.0, 30)
                .unwrap();
        assert!(large > small);
    }

    #[test]
    fn snapshot_populates_all_fields() {
        let snap = estimate_cpmm_impact_snapshot(20_000.0, 20_000.0, 30, 100.0, 1000.0, true, true);
        assert!(snap.buy_small_bps.is_some());
        assert!(snap.sell_small_bps.is_some());
        assert!(snap.buy_large_bps.unwrap() >= snap.buy_small_bps.unwrap());
        assert!(snap.sell_large_bps.unwrap() >= snap.sell_small_bps.unwrap());
        assert!(snap.approx);
        assert!(snap.quote_is_usd);
    }
}
