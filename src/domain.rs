use crate::constants::{MINT_BONK, MINT_JITOSOL, MINT_USDC, MINT_USDT, MINT_WSOL};
use serde::Deserialize;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct VenueId {
    pub dex: String,
    pub kind: String,
    pub display_name: String,
}

impl VenueId {
    pub fn new(dex: &str, kind: &str, display_name: &str) -> Self {
        Self {
            dex: dex.to_string(),
            kind: kind.to_string(),
            display_name: display_name.to_string(),
        }
    }

    pub fn slug(&self) -> String {
        format!("{}_{}", self.dex, self.kind)
    }
}

#[derive(Clone, Debug)]
pub struct VenueEvent {
    pub venue: VenueId,
    pub signature: String,
    pub pool_id: String,
    pub token_mint: String,
    pub quote_mint: String,
}

impl VenueEvent {
    pub fn dedupe_key(&self) -> String {
        format!("{}:{}:{}", self.venue.dex, self.venue.kind, self.pool_id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlowSide {
    Buy,
    Sell,
}

#[derive(Clone, Debug)]
pub struct PoolSwapFlowEvent {
    pub venue: VenueId,
    pub pool_id: String,
    pub side: FlowSide,
    pub quote_volume: f64,
    pub quote_liquidity: Option<f64>,
    pub mark_price_ratio: Option<f64>,
    pub ts_unix: u64,
}

impl PoolSwapFlowEvent {
    pub fn pool_dedupe_key(&self) -> String {
        format!("{}:{}:{}", self.venue.dex, self.venue.kind, self.pool_id)
    }
}

#[derive(Default, Clone, Debug)]
pub struct TokenMetadataLite {
    pub name: Option<String>,
    pub symbol: Option<String>,
}

#[derive(Clone, Debug)]
pub struct HolderShare {
    pub address: String,
    pub pct: f64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RugCheckTopHolder {
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub pct: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RugCheckRisk {
    pub name: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct RugCheckTokenMeta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub mutable: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct RugCheckFileMeta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct RugCheckReport {
    #[serde(default, rename = "score_normalised")]
    pub score_normalised: Option<u64>,
    #[serde(default)]
    pub rugged: Option<bool>,
    #[serde(default, rename = "totalMarketLiquidity")]
    pub total_market_liquidity: Option<f64>,
    #[serde(default, rename = "totalStableLiquidity")]
    pub total_stable_liquidity: Option<f64>,
    #[serde(default, rename = "totalLPProviders")]
    pub total_lp_providers: Option<u64>,
    #[serde(default, rename = "totalHolders")]
    pub total_holders: Option<u64>,
    #[serde(default, rename = "topHolders")]
    pub top_holders: Vec<RugCheckTopHolder>,
    #[serde(default)]
    pub risks: Vec<RugCheckRisk>,
    #[serde(default, rename = "tokenMeta")]
    pub token_meta: RugCheckTokenMeta,
    #[serde(default, rename = "fileMeta")]
    pub file_meta: RugCheckFileMeta,
}

pub fn known_symbol_for_mint(mint: &str) -> Option<&'static str> {
    match mint {
        MINT_WSOL => Some("SOL"),
        MINT_USDC => Some("USDC"),
        MINT_USDT => Some("USDT"),
        MINT_BONK => Some("BONK"),
        MINT_JITOSOL => Some("JITOSOL"),
        _ => None,
    }
}

pub fn short_mint(mint: &str) -> String {
    if mint.len() <= 8 {
        return mint.to_string();
    }
    format!("{}{}", &mint[0..4], &mint[mint.len() - 4..])
}

pub fn symbol_for_mint(mint: &str) -> String {
    known_symbol_for_mint(mint)
        .map(ToString::to_string)
        .unwrap_or_else(|| short_mint(mint))
}
