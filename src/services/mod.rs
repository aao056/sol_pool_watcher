pub mod analytics;
pub mod message;
pub mod metadata;
pub mod rugcheck;

use std::collections::HashSet;

pub fn select_token_and_quote_strict(
    mint0: &str,
    mint1: &str,
    allowed_quote_mints: &HashSet<String>,
) -> Option<(String, String)> {
    let m0_quote = allowed_quote_mints.contains(mint0);
    let m1_quote = allowed_quote_mints.contains(mint1);

    match (m0_quote, m1_quote) {
        (true, false) => Some((mint1.to_string(), mint0.to_string())),
        (false, true) => Some((mint0.to_string(), mint1.to_string())),
        _ => None,
    }
}

pub fn select_token_and_quote(
    mint0: &str,
    mint1: &str,
    allowed_quote_mints: &HashSet<String>,
) -> Option<(String, String)> {
    if let Some(pair) = select_token_and_quote_strict(mint0, mint1, allowed_quote_mints) {
        return Some(pair);
    }

    // Arb discovery mode needs to preserve all pools, including token/token pairs.
    // When no configured quote mint is present (or both are quote mints), return a
    // canonical lexicographic pair order so storage and cross-venue matching stay deterministic.
    if mint0 <= mint1 {
        Some((mint0.to_string(), mint1.to_string()))
    } else {
        Some((mint1.to_string(), mint0.to_string()))
    }
}
