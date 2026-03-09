use crate::constants::{
    DEFAULT_METEORA_DAMM_PROGRAM_ID, DEFAULT_METEORA_DLMM_PROGRAM_ID,
    DEFAULT_ORCA_WHIRLPOOL_PROGRAM_ID, DEFAULT_PANCAKESWAP_CLMM_PROGRAM_ID,
    DEFAULT_PUMPSWAP_PROGRAM_ID, DEFAULT_RAYDIUM_AMM_PROGRAM_ID, DEFAULT_RAYDIUM_CLMM_PROGRAM_ID,
    DEFAULT_RAYDIUM_CPMM_PROGRAM_ID, MINT_USDC, MINT_WSOL,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashSet;
use std::fs;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub venues: VenuesConfig,
    #[serde(default)]
    pub raydium: LegacyRaydiumConfig,
    #[serde(default)]
    pub tracking: TrackingConfig,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LegacyRaydiumConfig {
    pub clmm_program_id: Option<String>,
    pub allowed_quote_mints: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct VenuesConfig {
    #[serde(default)]
    pub raydium_clmm: RaydiumClmmConfig,
    #[serde(default)]
    pub pancakeswap_clmm: ToggleVenueConfig,
    #[serde(default)]
    pub raydium_cpmm: ToggleVenueConfig,
    #[serde(default)]
    pub raydium_amm: ToggleVenueConfig,
    #[serde(default)]
    pub meteora_damm: ToggleVenueConfig,
    #[serde(default)]
    pub meteora_dlmm: ToggleVenueConfig,
    #[serde(default)]
    pub pumpswap: ToggleVenueConfig,
    #[serde(default)]
    pub orca_whirlpool: ToggleVenueConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RaydiumClmmConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, alias = "clmm_program_id")]
    pub program_id: Option<String>,
    #[serde(default)]
    pub allowed_quote_mints: Option<Vec<String>>,
}

impl Default for RaydiumClmmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            program_id: None,
            allowed_quote_mints: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ToggleVenueConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub program_id: Option<String>,
    #[serde(default)]
    pub allowed_quote_mints: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrackingConfig {
    #[serde(default = "default_tracking_enabled")]
    pub enabled: bool,
    #[serde(default = "default_tracking_tick_interval_secs")]
    pub tick_interval_secs: u64,
    #[serde(default = "default_tracking_refresh_interval_secs")]
    pub refresh_interval_secs: u64,
    #[serde(default = "default_tracking_light_secs")]
    pub light_tracking_secs: u64,
    #[serde(default = "default_tracking_secs")]
    pub tracking_secs: u64,
    #[serde(default = "default_tracking_evict_after_secs")]
    pub evict_after_secs: u64,
    #[serde(default = "default_tracking_notify_cooldown_secs")]
    pub notify_cooldown_secs: u64,
    #[serde(default = "default_tracking_update_score_delta_threshold")]
    pub update_score_delta_threshold: u64,
    #[serde(default = "default_tracking_liquidity_drop_alert_ratio")]
    pub liquidity_drop_alert_ratio: f64,
    #[serde(default = "default_tracking_promote_min_score")]
    pub promote_min_score: u64,
    #[serde(default = "default_tracking_promote_min_liquidity_usd")]
    pub promote_min_liquidity_usd: f64,
    #[serde(default = "default_tracking_promote_require_low_risk")]
    pub promote_require_low_risk: bool,
    #[serde(default = "default_tracking_holders_refresh_interval_secs")]
    pub holders_refresh_interval_secs: u64,
    #[serde(default = "default_tracking_request_timeout_ms")]
    pub request_timeout_ms: u64,
    #[serde(default = "default_tracking_max_tracked_pools")]
    pub max_tracked_pools: u64,
    #[serde(default = "default_tracking_liquidity_history_secs")]
    pub liquidity_history_secs: u64,
    #[serde(default = "default_tracking_cpmm_assumed_fee_bps")]
    pub cpmm_assumed_fee_bps: u64,
    #[serde(default = "default_tracking_impact_small_usd")]
    pub impact_small_usd: f64,
    #[serde(default = "default_tracking_impact_large_usd")]
    pub impact_large_usd: f64,
    #[serde(default = "default_tracking_shadow_enabled")]
    pub shadow_enabled: bool,
    #[serde(default = "default_tracking_shadow_tp_bps")]
    pub shadow_take_profit_bps: u64,
    #[serde(default = "default_tracking_shadow_sl_bps")]
    pub shadow_stop_loss_bps: u64,
    #[serde(default = "default_tracking_shadow_max_hold_secs")]
    pub shadow_max_hold_secs: u64,
}

impl Default for TrackingConfig {
    fn default() -> Self {
        Self {
            enabled: default_tracking_enabled(),
            tick_interval_secs: default_tracking_tick_interval_secs(),
            refresh_interval_secs: default_tracking_refresh_interval_secs(),
            light_tracking_secs: default_tracking_light_secs(),
            tracking_secs: default_tracking_secs(),
            evict_after_secs: default_tracking_evict_after_secs(),
            notify_cooldown_secs: default_tracking_notify_cooldown_secs(),
            update_score_delta_threshold: default_tracking_update_score_delta_threshold(),
            liquidity_drop_alert_ratio: default_tracking_liquidity_drop_alert_ratio(),
            promote_min_score: default_tracking_promote_min_score(),
            promote_min_liquidity_usd: default_tracking_promote_min_liquidity_usd(),
            promote_require_low_risk: default_tracking_promote_require_low_risk(),
            holders_refresh_interval_secs: default_tracking_holders_refresh_interval_secs(),
            request_timeout_ms: default_tracking_request_timeout_ms(),
            max_tracked_pools: default_tracking_max_tracked_pools(),
            liquidity_history_secs: default_tracking_liquidity_history_secs(),
            cpmm_assumed_fee_bps: default_tracking_cpmm_assumed_fee_bps(),
            impact_small_usd: default_tracking_impact_small_usd(),
            impact_large_usd: default_tracking_impact_large_usd(),
            shadow_enabled: default_tracking_shadow_enabled(),
            shadow_take_profit_bps: default_tracking_shadow_tp_bps(),
            shadow_stop_loss_bps: default_tracking_shadow_sl_bps(),
            shadow_max_hold_secs: default_tracking_shadow_max_hold_secs(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_allowed_quote_mints() -> Vec<String> {
    vec![MINT_WSOL.to_string(), MINT_USDC.to_string()]
}

fn default_tracking_enabled() -> bool {
    true
}

fn default_tracking_tick_interval_secs() -> u64 {
    10
}

fn default_tracking_refresh_interval_secs() -> u64 {
    20
}

fn default_tracking_light_secs() -> u64 {
    60
}

fn default_tracking_secs() -> u64 {
    10 * 60
}

fn default_tracking_evict_after_secs() -> u64 {
    20 * 60
}

fn default_tracking_notify_cooldown_secs() -> u64 {
    60
}

fn default_tracking_update_score_delta_threshold() -> u64 {
    10
}

fn default_tracking_liquidity_drop_alert_ratio() -> f64 {
    0.15
}

fn default_tracking_promote_min_score() -> u64 {
    55
}

fn default_tracking_promote_min_liquidity_usd() -> f64 {
    20_000.0
}

fn default_tracking_promote_require_low_risk() -> bool {
    true
}

fn default_tracking_holders_refresh_interval_secs() -> u64 {
    120
}

fn default_tracking_request_timeout_ms() -> u64 {
    1500
}

fn default_tracking_max_tracked_pools() -> u64 {
    300
}

fn default_tracking_liquidity_history_secs() -> u64 {
    900
}

fn default_tracking_cpmm_assumed_fee_bps() -> u64 {
    30
}

fn default_tracking_impact_small_usd() -> f64 {
    100.0
}

fn default_tracking_impact_large_usd() -> f64 {
    1000.0
}

fn default_tracking_shadow_enabled() -> bool {
    true
}

fn default_tracking_shadow_tp_bps() -> u64 {
    2000
}

fn default_tracking_shadow_sl_bps() -> u64 {
    1200
}

fn default_tracking_shadow_max_hold_secs() -> u64 {
    20 * 60
}

pub fn parse_cfg(path: &str) -> Result<Config> {
    let raw = fs::read_to_string(path).with_context(|| format!("failed to read cfg: {path}"))?;
    let cfg: Config = toml::from_str(&raw).context("failed to parse cfg toml")?;
    Ok(cfg)
}

impl Config {
    pub fn raydium_cpmm_program_id(&self) -> String {
        self.venues
            .raydium_cpmm
            .program_id
            .clone()
            .unwrap_or_else(|| DEFAULT_RAYDIUM_CPMM_PROGRAM_ID.to_string())
    }

    pub fn raydium_cpmm_allowed_quote_mints(&self) -> HashSet<String> {
        normalize_mints(
            self.venues
                .raydium_cpmm
                .allowed_quote_mints
                .clone()
                .or_else(|| self.raydium.allowed_quote_mints.clone()),
        )
    }

    pub fn raydium_amm_program_id(&self) -> String {
        self.venues
            .raydium_amm
            .program_id
            .clone()
            .unwrap_or_else(|| DEFAULT_RAYDIUM_AMM_PROGRAM_ID.to_string())
    }

    pub fn raydium_amm_allowed_quote_mints(&self) -> HashSet<String> {
        normalize_mints(
            self.venues
                .raydium_amm
                .allowed_quote_mints
                .clone()
                .or_else(|| self.raydium.allowed_quote_mints.clone()),
        )
    }

    pub fn raydium_clmm_program_id(&self) -> String {
        self.venues
            .raydium_clmm
            .program_id
            .clone()
            .or_else(|| self.raydium.clmm_program_id.clone())
            .unwrap_or_else(|| DEFAULT_RAYDIUM_CLMM_PROGRAM_ID.to_string())
    }

    pub fn raydium_clmm_allowed_quote_mints(&self) -> HashSet<String> {
        normalize_mints(
            self.venues
                .raydium_clmm
                .allowed_quote_mints
                .clone()
                .or_else(|| self.raydium.allowed_quote_mints.clone()),
        )
    }

    pub fn pancakeswap_clmm_program_id(&self) -> String {
        self.venues
            .pancakeswap_clmm
            .program_id
            .clone()
            .unwrap_or_else(|| DEFAULT_PANCAKESWAP_CLMM_PROGRAM_ID.to_string())
    }

    pub fn pancakeswap_clmm_allowed_quote_mints(&self) -> HashSet<String> {
        normalize_mints(
            self.venues
                .pancakeswap_clmm
                .allowed_quote_mints
                .clone()
                .or_else(|| self.raydium.allowed_quote_mints.clone()),
        )
    }

    pub fn meteora_dlmm_program_id(&self) -> String {
        self.venues
            .meteora_dlmm
            .program_id
            .clone()
            .unwrap_or_else(|| DEFAULT_METEORA_DLMM_PROGRAM_ID.to_string())
    }

    pub fn meteora_dlmm_allowed_quote_mints(&self) -> HashSet<String> {
        normalize_mints(
            self.venues
                .meteora_dlmm
                .allowed_quote_mints
                .clone()
                .or_else(|| self.raydium.allowed_quote_mints.clone()),
        )
    }

    pub fn meteora_damm_program_id(&self) -> String {
        self.venues
            .meteora_damm
            .program_id
            .clone()
            .unwrap_or_else(|| DEFAULT_METEORA_DAMM_PROGRAM_ID.to_string())
    }

    pub fn meteora_damm_allowed_quote_mints(&self) -> HashSet<String> {
        normalize_mints(
            self.venues
                .meteora_damm
                .allowed_quote_mints
                .clone()
                .or_else(|| self.raydium.allowed_quote_mints.clone()),
        )
    }

    pub fn pumpswap_program_id(&self) -> String {
        self.venues
            .pumpswap
            .program_id
            .clone()
            .unwrap_or_else(|| DEFAULT_PUMPSWAP_PROGRAM_ID.to_string())
    }

    pub fn pumpswap_allowed_quote_mints(&self) -> HashSet<String> {
        normalize_mints(
            self.venues
                .pumpswap
                .allowed_quote_mints
                .clone()
                .or_else(|| self.raydium.allowed_quote_mints.clone()),
        )
    }

    pub fn orca_whirlpool_program_id(&self) -> String {
        self.venues
            .orca_whirlpool
            .program_id
            .clone()
            .unwrap_or_else(|| DEFAULT_ORCA_WHIRLPOOL_PROGRAM_ID.to_string())
    }

    pub fn orca_whirlpool_allowed_quote_mints(&self) -> HashSet<String> {
        normalize_mints(
            self.venues
                .orca_whirlpool
                .allowed_quote_mints
                .clone()
                .or_else(|| self.raydium.allowed_quote_mints.clone()),
        )
    }
}

fn normalize_mints(input: Option<Vec<String>>) -> HashSet<String> {
    input
        .unwrap_or_else(default_allowed_quote_mints)
        .into_iter()
        .map(|mint| mint.trim().to_string())
        .filter(|mint| !mint.is_empty())
        .collect()
}
