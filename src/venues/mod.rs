pub mod meteora;
pub mod orca;
pub mod pumpswap;
pub mod raydium;

use crate::config::Config;
use crate::domain::{PoolSwapFlowEvent, VenueEvent};
use crate::venues::meteora::damm::MeteoraDammWatcher;
use crate::venues::meteora::dlmm::MeteoraDlmmWatcher;
use crate::venues::orca::OrcaWhirlpoolWatcher;
use crate::venues::pumpswap::PumpSwapWatcher;
use crate::venues::raydium::amm::RaydiumAmmWatcher;
use crate::venues::raydium::clmm::RaydiumClmmWatcher;
use crate::venues::raydium::cpmm::RaydiumCpmmWatcher;
use anyhow::{Context, Result};
use reqwest::Client as HttpClient;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;
use tracing::{info, warn};

#[derive(Clone)]
pub struct VenueRuntime {
    pub ws_url: String,
    pub rpc_url: String,
    pub rpc_client: Arc<RpcClient>,
    pub http_client: HttpClient,
    pub event_tx: UnboundedSender<VenueEvent>,
    pub flow_tx: UnboundedSender<PoolSwapFlowEvent>,
}

pub trait VenueWatcher: Send {
    fn name(&self) -> &'static str;
    fn spawn(self: Box<Self>, runtime: VenueRuntime) -> JoinHandle<()>;
}

pub fn build_watcher_by_name(cfg: &Config, name: &str) -> Result<Option<Box<dyn VenueWatcher>>> {
    let watcher: Option<Box<dyn VenueWatcher>> = match name {
        "raydium_clmm" if cfg.venues.raydium_clmm.enabled => {
            let clmm_program_id = Pubkey::from_str(&cfg.raydium_clmm_program_id())
                .context("invalid raydium clmm program_id")?;
            let allowed_quote_mints = cfg.raydium_clmm_allowed_quote_mints();
            Some(Box::new(RaydiumClmmWatcher::new(
                clmm_program_id,
                allowed_quote_mints,
            )))
        }
        "pancakeswap_clmm" if cfg.venues.pancakeswap_clmm.enabled => {
            let clmm_program_id = Pubkey::from_str(&cfg.pancakeswap_clmm_program_id())
                .context("invalid pancakeswap clmm program_id")?;
            let allowed_quote_mints = cfg.pancakeswap_clmm_allowed_quote_mints();
            Some(Box::new(RaydiumClmmWatcher::new_with_venue(
                clmm_program_id,
                allowed_quote_mints,
                crate::domain::VenueId::new("pancakeswap", "clmm", "PancakeSwap CLMM"),
                "pancakeswap_clmm",
            )))
        }
        "raydium_amm" if cfg.venues.raydium_amm.enabled => {
            let amm_program_id = Pubkey::from_str(&cfg.raydium_amm_program_id())
                .context("invalid raydium amm program_id")?;
            let allowed_quote_mints = cfg.raydium_amm_allowed_quote_mints();
            Some(Box::new(RaydiumAmmWatcher::new(
                amm_program_id,
                allowed_quote_mints,
            )))
        }
        "raydium_cpmm" if cfg.venues.raydium_cpmm.enabled => {
            let cpmm_program_id = Pubkey::from_str(&cfg.raydium_cpmm_program_id())
                .context("invalid raydium cpmm program_id")?;
            let allowed_quote_mints = cfg.raydium_cpmm_allowed_quote_mints();
            Some(Box::new(RaydiumCpmmWatcher::new(
                cpmm_program_id,
                allowed_quote_mints,
            )))
        }
        "meteora_damm" if cfg.venues.meteora_damm.enabled => {
            let program_id = Pubkey::from_str(&cfg.meteora_damm_program_id())
                .context("invalid meteora damm program_id")?;
            let allowed_quote_mints = cfg.meteora_damm_allowed_quote_mints();
            Some(Box::new(MeteoraDammWatcher::new(
                program_id,
                allowed_quote_mints,
            )))
        }
        "meteora_dlmm" if cfg.venues.meteora_dlmm.enabled => {
            let program_id = Pubkey::from_str(&cfg.meteora_dlmm_program_id())
                .context("invalid meteora dlmm program_id")?;
            let allowed_quote_mints = cfg.meteora_dlmm_allowed_quote_mints();
            Some(Box::new(MeteoraDlmmWatcher::new(
                program_id,
                allowed_quote_mints,
            )))
        }
        "pumpswap" if cfg.venues.pumpswap.enabled => {
            let program_id = Pubkey::from_str(&cfg.pumpswap_program_id())
                .context("invalid pumpswap program_id")?;
            let allowed_quote_mints = cfg.pumpswap_allowed_quote_mints();
            Some(Box::new(PumpSwapWatcher::new(
                program_id,
                allowed_quote_mints,
            )))
        }
        "orca_whirlpool" if cfg.venues.orca_whirlpool.enabled => {
            let program_id = Pubkey::from_str(&cfg.orca_whirlpool_program_id())
                .context("invalid orca whirlpool program_id")?;
            let allowed_quote_mints = cfg.orca_whirlpool_allowed_quote_mints();
            Some(Box::new(OrcaWhirlpoolWatcher::new(
                program_id,
                allowed_quote_mints,
            )))
        }
        _ => None,
    };

    Ok(watcher)
}

pub fn build_watchers(cfg: &Config) -> Result<Vec<Box<dyn VenueWatcher>>> {
    let mut watchers: Vec<Box<dyn VenueWatcher>> = Vec::new();

    if cfg.venues.raydium_clmm.enabled {
        let clmm_program_id = Pubkey::from_str(&cfg.raydium_clmm_program_id())
            .context("invalid raydium clmm program_id")?;
        let allowed_quote_mints = cfg.raydium_clmm_allowed_quote_mints();
        watchers.push(Box::new(RaydiumClmmWatcher::new(
            clmm_program_id,
            allowed_quote_mints,
        )));
    }

    if cfg.venues.pancakeswap_clmm.enabled {
        let clmm_program_id = Pubkey::from_str(&cfg.pancakeswap_clmm_program_id())
            .context("invalid pancakeswap clmm program_id")?;
        let allowed_quote_mints = cfg.pancakeswap_clmm_allowed_quote_mints();
        watchers.push(Box::new(RaydiumClmmWatcher::new_with_venue(
            clmm_program_id,
            allowed_quote_mints,
            crate::domain::VenueId::new("pancakeswap", "clmm", "PancakeSwap CLMM"),
            "pancakeswap_clmm",
        )));
    }

    if cfg.venues.raydium_amm.enabled {
        let amm_program_id = Pubkey::from_str(&cfg.raydium_amm_program_id())
            .context("invalid raydium amm program_id")?;
        let allowed_quote_mints = cfg.raydium_amm_allowed_quote_mints();
        watchers.push(Box::new(RaydiumAmmWatcher::new(
            amm_program_id,
            allowed_quote_mints,
        )));
    }

    if cfg.venues.raydium_cpmm.enabled {
        let cpmm_program_id = Pubkey::from_str(&cfg.raydium_cpmm_program_id())
            .context("invalid raydium cpmm program_id")?;
        let allowed_quote_mints = cfg.raydium_cpmm_allowed_quote_mints();
        watchers.push(Box::new(RaydiumCpmmWatcher::new(
            cpmm_program_id,
            allowed_quote_mints,
        )));
    }

    if cfg.venues.meteora_damm.enabled {
        let program_id = Pubkey::from_str(&cfg.meteora_damm_program_id())
            .context("invalid meteora damm program_id")?;
        let allowed_quote_mints = cfg.meteora_damm_allowed_quote_mints();
        watchers.push(Box::new(MeteoraDammWatcher::new(
            program_id,
            allowed_quote_mints,
        )));
    }

    if cfg.venues.meteora_dlmm.enabled {
        let program_id = Pubkey::from_str(&cfg.meteora_dlmm_program_id())
            .context("invalid meteora dlmm program_id")?;
        let allowed_quote_mints = cfg.meteora_dlmm_allowed_quote_mints();
        watchers.push(Box::new(MeteoraDlmmWatcher::new(
            program_id,
            allowed_quote_mints,
        )));
    }

    if cfg.venues.pumpswap.enabled {
        let program_id =
            Pubkey::from_str(&cfg.pumpswap_program_id()).context("invalid pumpswap program_id")?;
        let allowed_quote_mints = cfg.pumpswap_allowed_quote_mints();
        watchers.push(Box::new(PumpSwapWatcher::new(
            program_id,
            allowed_quote_mints,
        )));
    }

    if cfg.venues.orca_whirlpool.enabled {
        let program_id = Pubkey::from_str(&cfg.orca_whirlpool_program_id())
            .context("invalid orca whirlpool program_id")?;
        let allowed_quote_mints = cfg.orca_whirlpool_allowed_quote_mints();
        watchers.push(Box::new(OrcaWhirlpoolWatcher::new(
            program_id,
            allowed_quote_mints,
        )));
    }

    if watchers.is_empty() {
        warn!("no venue watchers enabled in config");
    } else {
        let enabled = watchers.iter().map(|w| w.name()).collect::<Vec<_>>();
        info!(enabled = ?enabled, "venue watchers configured");
    }

    Ok(watchers)
}
