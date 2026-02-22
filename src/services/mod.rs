pub mod analytics;
pub mod message;
pub mod metadata;
pub mod rugcheck;

use std::collections::HashSet;

pub fn select_token_and_quote(
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
