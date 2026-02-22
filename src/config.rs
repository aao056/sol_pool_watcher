use crate::constants::{
    DEFAULT_METEORA_DAMM_PROGRAM_ID, DEFAULT_METEORA_DLMM_PROGRAM_ID,
    DEFAULT_ORCA_WHIRLPOOL_PROGRAM_ID, DEFAULT_PUMPSWAP_PROGRAM_ID,
    DEFAULT_RAYDIUM_CLMM_PROGRAM_ID, MINT_USDC, MINT_WSOL,
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

fn default_true() -> bool {
    true
}

fn default_allowed_quote_mints() -> Vec<String> {
    vec![MINT_WSOL.to_string(), MINT_USDC.to_string()]
}

pub fn parse_cfg(path: &str) -> Result<Config> {
    let raw = fs::read_to_string(path).with_context(|| format!("failed to read cfg: {path}"))?;
    let cfg: Config = toml::from_str(&raw).context("failed to parse cfg toml")?;
    Ok(cfg)
}

impl Config {
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
