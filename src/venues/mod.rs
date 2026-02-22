pub mod meteora;
pub mod orca;
pub mod pumpswap;
pub mod raydium;
mod stub;

use crate::config::Config;
use crate::domain::{VenueEvent, VenueId};
use crate::venues::meteora::damm::MeteoraDammWatcher;
use crate::venues::meteora::dlmm::MeteoraDlmmWatcher;
use crate::venues::orca::OrcaWhirlpoolWatcher;
use crate::venues::pumpswap::PumpSwapWatcher;
use crate::venues::raydium::clmm::RaydiumClmmWatcher;
use crate::venues::stub::StubVenueWatcher;
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
}

pub trait VenueWatcher: Send {
    fn name(&self) -> &'static str;
    fn spawn(self: Box<Self>, runtime: VenueRuntime) -> JoinHandle<()>;
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

    if cfg.venues.raydium_amm.enabled {
        watchers.push(Box::new(StubVenueWatcher::new(
            VenueId::new("raydium", "amm", "Raydium AMM"),
            cfg.venues.raydium_amm.program_id.clone(),
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
