use crate::config::TrackingConfig;
use crate::constants::MAX_SEEN_EVENTS;
use crate::domain::{FlowSide, PoolSwapFlowEvent, VenueEvent, symbol_for_mint};
use crate::notifier::TelegramNotifier;
use crate::pool_store::PoolStore;
use crate::services::analytics::fetch_top_holders;
use crate::services::message::build_telegram_message;
use crate::services::metadata::fetch_token_metadata;
use crate::services::rugcheck::{fetch_rugcheck_report, holders_from_rugcheck};
use crate::tracking::model::HolderAggregates;
use crate::tracking::rolling::SwapSide;
use crate::tracking::{PoolTrackingSeed, TrackingManager, rug_snapshot_from_report};
use reqwest::Client as HttpClient;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{info, warn};

fn remember_key(
    seen: &mut HashSet<String>,
    order: &mut VecDeque<String>,
    value: &str,
    cap: usize,
) -> bool {
    if !seen.insert(value.to_string()) {
        return false;
    }
    order.push_back(value.to_string());
    while order.len() > cap {
        if let Some(evicted) = order.pop_front() {
            seen.remove(&evicted);
        }
    }
    true
}

pub struct EventPipeline {
    pub http_client: HttpClient,
    pub rpc_client: Arc<RpcClient>,
    pub rpc_url: String,
    pub telegram: Option<TelegramNotifier>,
    pub pool_store: Option<Arc<PoolStore>>,
    pub sim_mode: bool,
    pub tracking_cfg: TrackingConfig,
}

impl EventPipeline {
    pub async fn run(
        self,
        mut rx: UnboundedReceiver<VenueEvent>,
        mut flow_rx: UnboundedReceiver<PoolSwapFlowEvent>,
    ) {
        let EventPipeline {
            http_client,
            rpc_client,
            rpc_url,
            telegram,
            pool_store,
            sim_mode,
            tracking_cfg,
        } = self;

        let mut seen_events: HashSet<String> = HashSet::new();
        let mut seen_order: VecDeque<String> = VecDeque::new();
        let mut tracking_manager = if sim_mode {
            let mut cfg = tracking_cfg;
            cfg.enabled = true;
            TrackingManager::new(cfg, http_client.clone(), rpc_url.clone(), telegram.clone())
        } else {
            None
        };
        let tracking_enabled = tracking_manager.is_some();
        let tracking_tick_secs = tracking_manager
            .as_ref()
            .map(|m| m.poll_interval_secs())
            .unwrap_or(60);
        let mut tracking_interval = tokio::time::interval(Duration::from_secs(tracking_tick_secs));
        tracking_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut event_stream_open = true;
        let mut flow_stream_open = true;

        loop {
            tokio::select! {
                maybe_event = rx.recv(), if event_stream_open => {
                    let Some(event) = maybe_event else {
                        event_stream_open = false;
                        info!("event stream closed");
                        continue;
                    };

                    if !remember_key(
                        &mut seen_events,
                        &mut seen_order,
                        &event.dedupe_key(),
                        MAX_SEEN_EVENTS,
                    ) {
                        continue;
                    }

                    info!(
                        venue = %event.venue.slug(),
                        signature = %event.signature,
                        pool_id = %event.pool_id,
                        token_mint = %event.token_mint,
                        quote_mint = %event.quote_mint,
                        "new pool detected"
                    );

                    if let Some(store) = &pool_store {
                        match store.upsert_event(&event) {
                            Ok(status) => {
                                if status.became_eligible {
                                    info!(
                                        token_mint = %event.token_mint,
                                        quote_mint = %event.quote_mint,
                                        dex_count = status.dex_count,
                                        pool_count = status.pool_count,
                                        "pair reached multi-dex threshold and is now eligible for arb monitoring"
                                    );

                                    if !sim_mode
                                        && let Some(notifier) = &telegram
                                    {
                                        let msg = format!(
                                            "Multi-dex candidate detected\n\nToken: {}\nQuote: {}\nDexes: {}\nPools: {}\nLatest pool: {}\nVenue: {}",
                                            event.token_mint,
                                            event.quote_mint,
                                            status.dex_count,
                                            status.pool_count,
                                            event.pool_id,
                                            event.venue.display_name,
                                        );
                                        notifier.send_event(&msg).await;
                                    }
                                } else if status.eligible {
                                    info!(
                                        token_mint = %event.token_mint,
                                        quote_mint = %event.quote_mint,
                                        dex_count = status.dex_count,
                                        pool_count = status.pool_count,
                                        "pair remains eligible for arb monitoring"
                                    );
                                }
                            }
                            Err(err) => {
                                warn!(
                                    ?err,
                                    pool_id = %event.pool_id,
                                    token_mint = %event.token_mint,
                                    quote_mint = %event.quote_mint,
                                    "failed to persist pool event to DB"
                                );
                            }
                        }
                    }

                    let token_meta_fut = fetch_token_metadata(rpc_client.as_ref(), &event.token_mint);
                    let rugcheck_fut = fetch_rugcheck_report(&http_client, &event.token_mint);
                    let (token_meta, rugcheck) = tokio::join!(token_meta_fut, rugcheck_fut);

                    let mut top_holders = rugcheck
                        .as_ref()
                        .map(|report| holders_from_rugcheck(report, 10))
                        .unwrap_or_default();
                    if top_holders.is_empty() {
                        top_holders = fetch_top_holders(&http_client, &rpc_url, &event.token_mint, 10)
                            .await
                            .unwrap_or_default();
                    }

                    if let Some(manager) = tracking_manager.as_mut() {
                        let token_symbol = token_meta
                            .as_ref()
                            .and_then(|m| m.symbol.clone())
                            .unwrap_or_else(|| symbol_for_mint(&event.token_mint));
                        let token_name = token_meta
                            .as_ref()
                            .and_then(|m| m.name.clone())
                            .unwrap_or_else(|| token_symbol.clone());
                        let seed = PoolTrackingSeed {
                            event: event.clone(),
                            detected_at_unix: now_unix_secs(),
                            token_name,
                            token_symbol,
                            quote_symbol: symbol_for_mint(&event.quote_mint),
                            top_holders: top_holders.clone(),
                            holder_aggregates: HolderAggregates::from_holders(&top_holders),
                            rug: rugcheck
                                .as_ref()
                                .map(rug_snapshot_from_report)
                                .unwrap_or_default(),
                        };
                        manager.on_new_pool(seed).await;
                    }

                    if !sim_mode
                        && let Some(notifier) = &telegram
                    {
                        let message = build_telegram_message(
                            &event,
                            token_meta.as_ref(),
                            &top_holders,
                            rugcheck.as_ref(),
                        );
                        notifier.send_event(&message).await;
                    }
                }
                maybe_flow = flow_rx.recv(), if flow_stream_open => {
                    let Some(flow_event) = maybe_flow else {
                        flow_stream_open = false;
                        info!("pool flow stream closed");
                        continue;
                    };
                    if let Some(manager) = tracking_manager.as_mut() {
                        let side = match flow_event.side {
                            FlowSide::Buy => SwapSide::Buy,
                            FlowSide::Sell => SwapSide::Sell,
                        };
                        manager.on_swap_quote_flow(
                            &flow_event.pool_dedupe_key(),
                            side,
                            flow_event.quote_volume,
                            flow_event.quote_liquidity,
                            flow_event.mark_price_ratio,
                            flow_event.ts_unix,
                        );
                    }
                }
                _ = tracking_interval.tick(), if tracking_enabled => {
                    if let Some(manager) = tracking_manager.as_mut() {
                        manager.on_tick().await;
                    }
                }
            }

            if !event_stream_open && !flow_stream_open {
                break;
            }
        }

        info!("event pipeline stopped");
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
