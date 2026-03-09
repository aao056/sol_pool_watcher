mod config;
mod constants;
mod domain;
mod notifier;
mod pipeline;
mod pool_store;
mod services;
mod tracking;
mod venues;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use config::parse_cfg;
use constants::{DEFAULT_RPC_TIMEOUT_SECS, HEALTHCHECK_INTERVAL_SECS};
use notifier::TelegramNotifier;
use pipeline::EventPipeline;
use pool_store::PoolStore;
use reqwest::Client as HttpClient;
use serde_json::{Value, json};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::time::Duration;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use venues::{VenueRuntime, build_watcher_by_name, build_watchers};

#[derive(Parser, Debug)]
#[command(name = "sol_pool_listener")]
#[command(about = "Multi-venue Solana new-pool listener")]
struct Cli {
    #[arg(long, default_value = "cfg.toml")]
    cfg: String,
    #[arg(long, value_enum, default_value_t = AppMode::Normal)]
    mode: AppMode,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AppMode {
    Normal,
    Sim,
}

fn required_env(key: &str) -> Result<String> {
    let value = std::env::var(key)
        .with_context(|| format!("missing required environment variable {key}"))?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("environment variable {key} is empty");
    }
    Ok(trimmed.to_string())
}

fn env_or_default(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => default.to_string(),
    }
}

fn helius_endpoints(api_key: &str) -> (String, String) {
    (
        format!("https://mainnet.helius-rpc.com/?api-key={api_key}"),
        format!("wss://mainnet.helius-rpc.com/?api-key={api_key}"),
    )
}

async fn fetch_rpc_slot(http_client: &HttpClient, rpc_url: &str) -> Result<u64> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getSlot",
        "params": [{"commitment": "confirmed"}]
    });

    let response = http_client
        .post(rpc_url)
        .json(&body)
        .send()
        .await
        .context("healthcheck getSlot request failed")?;
    let status = response.status();
    let payload: Value = response
        .json()
        .await
        .context("healthcheck getSlot json decode failed")?;

    if !status.is_success() {
        anyhow::bail!("healthcheck getSlot HTTP {}", status);
    }
    if let Some(err) = payload.get("error") {
        anyhow::bail!("healthcheck getSlot rpc error: {err}");
    }

    payload
        .get("result")
        .and_then(Value::as_u64)
        .context("healthcheck getSlot missing numeric result")
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let filter = match std::env::var("RUST_LOG").as_deref() {
        Ok("true") | Ok("1") => EnvFilter::new("debug"),
        Ok("false") | Ok("0") => EnvFilter::new("info"),
        Ok(other) => EnvFilter::new(other),
        Err(_) => EnvFilter::new("info"),
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let args = Cli::parse();
    let cfg = parse_cfg(&args.cfg)?;
    let helius_api_key = required_env("HELIUS_API_KEY")?;
    let (rpc_url, wss_url) = helius_endpoints(&helius_api_key);
    let sim_mode = matches!(args.mode, AppMode::Sim);
    let mut tracking_cfg = cfg.tracking.clone();
    if sim_mode && !tracking_cfg.enabled {
        tracking_cfg.enabled = true;
        info!("sim mode enabled: overriding tracking.enabled=false -> true");
    }

    let rpc_client = Arc::new(RpcClient::new(rpc_url.clone()));
    let http_client = HttpClient::builder()
        .timeout(Duration::from_secs(DEFAULT_RPC_TIMEOUT_SECS))
        .build()
        .context("failed to create HTTP client")?;

    let telegram = TelegramNotifier::from_env();
    let pool_db_path = env_or_default("POOL_DB_PATH", "data/pools.db");
    let pool_store = Arc::new(PoolStore::open(&pool_db_path)?);

    let watchers = build_watchers(&cfg)?;
    if watchers.is_empty() {
        warn!("no active venue watchers; exiting");
        return Ok(());
    }

    info!(
        cfg = %args.cfg,
        rpc_url = "https://mainnet.helius-rpc.com/?api-key=***",
        ws_url = "wss://mainnet.helius-rpc.com/?api-key=***",
        pool_db_path = %pool_db_path,
        mode = ?args.mode,
        telegram_enabled = telegram.is_some(),
        "starting multi-venue listener"
    );

    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (flow_tx, flow_rx) = tokio::sync::mpsc::unbounded_channel();
    let health_telegram = telegram.clone();

    let runtime = VenueRuntime {
        ws_url: wss_url.clone(),
        rpc_url: rpc_url.clone(),
        rpc_client: rpc_client.clone(),
        http_client: http_client.clone(),
        event_tx: event_tx.clone(),
        flow_tx: flow_tx.clone(),
    };
    let health_http_client = http_client.clone();
    let mut watcher_tasks = Vec::new();
    for watcher in watchers {
        let name = watcher.name().to_string();
        watcher_tasks.push((name, watcher.spawn(runtime.clone())));
    }
    drop(event_tx);
    drop(flow_tx);

    let pipeline = EventPipeline {
        http_client,
        rpc_client,
        rpc_url: rpc_url.clone(),
        telegram,
        pool_store: Some(pool_store),
        sim_mode,
        tracking_cfg,
    };
    let pipeline_task = tokio::spawn(pipeline.run(event_rx, flow_rx));

    let mut health_interval = tokio::time::interval(Duration::from_secs(HEALTHCHECK_INTERVAL_SECS));
    health_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut consecutive_rpc_failures = 0u64;
    let mut reported_dead_watchers: HashSet<String> = HashSet::new();
    let mut pipeline_dead_reported = false;

    info!(
        interval_secs = HEALTHCHECK_INTERVAL_SECS,
        "listeners running; press Ctrl-C to stop"
    );
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("ctrl-c received, shutting down");
                break;
            }
            _ = health_interval.tick() => {
                match fetch_rpc_slot(&health_http_client, &rpc_url).await {
                    Ok(slot) => {
                        consecutive_rpc_failures = 0;
                        info!(slot, "healthcheck rpc ok");
                    }
                    Err(err) => {
                        consecutive_rpc_failures += 1;
                        warn!(?err, consecutive_rpc_failures, "healthcheck rpc failed");
                    }
                }

                for (name, task) in &mut watcher_tasks {
                    if !task.is_finished() {
                        continue;
                    }

                    if reported_dead_watchers.insert(name.clone()) {
                        warn!(watcher = %name, "watcher task is no longer running");
                        if let Some(tg) = &health_telegram {
                            let msg = format!("⚠️ watcher down: {name}");
                            tg.send_error(&msg).await;
                        }
                    }

                    match build_watcher_by_name(&cfg, name) {
                        Ok(Some(watcher)) => {
                            warn!(watcher = %name, "attempting watcher restart");
                            *task = watcher.spawn(runtime.clone());
                            reported_dead_watchers.remove(name);
                            info!(watcher = %name, "watcher task restarted");
                        }
                        Ok(None) => {
                            warn!(watcher = %name, "watcher restart skipped (watcher disabled or unknown)");
                        }
                        Err(err) => {
                            warn!(watcher = %name, ?err, "watcher restart failed");
                            if let Some(tg) = &health_telegram {
                                let msg = format!("❌ watcher restart failed: {name}\n{err:#}");
                                tg.send_error(&msg).await;
                            }
                        }
                    }
                }

                if pipeline_task.is_finished() && !pipeline_dead_reported {
                    pipeline_dead_reported = true;
                    warn!("pipeline task is no longer running");
                    if let Some(tg) = &health_telegram {
                        tg.send_error("❌ pipeline task is no longer running").await;
                    }
                }

                let watchers_alive = watcher_tasks.iter().filter(|(_, task)| !task.is_finished()).count();
                info!(
                    watchers_alive,
                    watchers_total = watcher_tasks.len(),
                    pipeline_alive = !pipeline_task.is_finished(),
                    consecutive_rpc_failures,
                    "listener health snapshot"
                );
            }
        }
    }

    for (_, task) in watcher_tasks {
        task.abort();
    }
    pipeline_task.abort();

    Ok(())
}
