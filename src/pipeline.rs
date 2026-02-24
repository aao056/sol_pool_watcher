use crate::constants::MAX_SEEN_EVENTS;
use crate::domain::{
    FlowSide, PoolSwapFlowEvent, RugCheckReport, TokenMetadataLite, VenueEvent, symbol_for_mint,
};
use crate::notifier::TelegramNotifier;
use crate::pool_registry::{PoolRegistry, build_cross_venue_alert};
use crate::services::analytics::fetch_top_holders;
use crate::services::message::build_telegram_message;
use crate::services::metadata::fetch_token_metadata;
use crate::services::rugcheck::{fetch_rugcheck_report, holders_from_rugcheck};
use crate::tracking::model::{HolderAggregates, RugSnapshot};
use crate::tracking::rolling::SwapSide;
use crate::tracking::{PoolTrackingSeed, TrackingManager, rug_snapshot_from_report};
use reqwest::Client as HttpClient;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Duration;
use tracing::info;

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
    pub pool_registry: Option<PoolRegistry>,
    pub tracking: Option<TrackingManager>,
}

impl EventPipeline {
    pub async fn run(
        mut self,
        mut rx: UnboundedReceiver<VenueEvent>,
        mut flow_rx: UnboundedReceiver<PoolSwapFlowEvent>,
    ) {
        let mut seen_events: HashSet<String> = HashSet::new();
        let mut seen_order: VecDeque<String> = VecDeque::new();

        let mut tracking_tick = self
            .tracking
            .as_ref()
            .map(|t| tokio::time::interval(Duration::from_secs(t.poll_interval_secs())));
        if let Some(interval) = &mut tracking_tick {
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        }

        loop {
            tokio::select! {
                maybe_event = rx.recv() => {
                    let Some(event) = maybe_event else {
                        break;
                    };

                    if !remember_key(
                        &mut seen_events,
                        &mut seen_order,
                        &event.dedupe_key(),
                        MAX_SEEN_EVENTS,
                    ) {
                        continue;
                    }

                    let detected_at_unix = now_unix_secs();

                    if let Some(registry) = &self.pool_registry {
                        match registry.insert_pool_and_match(&event, detected_at_unix) {
                            Ok(Some(match_info)) => {
                                match registry
                                    .enqueue_cross_venue_candidate(&match_info, detected_at_unix)
                                {
                                    Ok(candidate_id) => {
                                        info!(
                                            candidate_id,
                                            token_mint = %event.token_mint,
                                            quote_mint = %event.quote_mint,
                                            venues_count = match_info.unique_venues.len(),
                                            "arb candidate queued"
                                        );
                                    }
                                    Err(err) => {
                                        info!(?err, "failed to queue arb candidate");
                                    }
                                }
                                info!(
                                    venue = %event.venue.slug(),
                                    pool_id = %event.pool_id,
                                    token_mint = %event.token_mint,
                                    quote_mint = %event.quote_mint,
                                    venues = ?match_info.unique_venues,
                                    "cross-venue pair detected"
                                );
                                if let Some(notifier) = &self.telegram {
                                    let alert = build_cross_venue_alert(&event, &match_info);
                                    notifier.send_event(&alert).await;
                                }
                            }
                            Ok(None) => {}
                            Err(err) => {
                                info!(?err, "pool registry insert/query failed");
                            }
                        }
                    }

                    let token_meta_fut = fetch_token_metadata(self.rpc_client.as_ref(), &event.token_mint);
                    let rugcheck_fut = fetch_rugcheck_report(&self.http_client, &event.token_mint);
                    let (token_meta, rugcheck) = tokio::join!(token_meta_fut, rugcheck_fut);

                    let mut top_holders = rugcheck
                        .as_ref()
                        .map(|report| holders_from_rugcheck(report, 10))
                        .unwrap_or_default();
                    if top_holders.is_empty() {
                        top_holders =
                            fetch_top_holders(&self.http_client, &self.rpc_url, &event.token_mint, 10)
                                .await
                                .unwrap_or_default();
                    }

                    let token_symbol = symbol_for_mint(&event.token_mint);
                    let quote_symbol = symbol_for_mint(&event.quote_mint);
                    let cfg_symbol = format!("{token_symbol}_{quote_symbol}");

                    info!(
                        venue = %event.venue.slug(),
                        signature = %event.signature,
                        pool_id = %event.pool_id,
                        token_mint = %event.token_mint,
                        quote_mint = %event.quote_mint,
                        cfg_symbol = %cfg_symbol,
                        "new pool detected"
                    );

                    if let Some(tracker) = self.tracking.as_mut() {
                        let seed = build_tracking_seed(&event, token_meta.as_ref(), rugcheck.as_ref(), &top_holders);
                        // Preserve registry timestamp for tracker timing consistency.
                        let mut seed = seed;
                        seed.detected_at_unix = detected_at_unix;
                        tracker.on_new_pool(seed).await;
                    } else if let Some(notifier) = &self.telegram {
                        let message = build_telegram_message(
                            &event,
                            token_meta.as_ref(),
                            &top_holders,
                            rugcheck.as_ref(),
                        );
                        notifier.send_event(&message).await;
                    }
                }
                maybe_flow = flow_rx.recv() => {
                    let Some(flow) = maybe_flow else {
                        continue;
                    };

                    if let Some(tracker) = self.tracking.as_mut() {
                        let side = match flow.side {
                            FlowSide::Buy => SwapSide::Buy,
                            FlowSide::Sell => SwapSide::Sell,
                        };
                        tracker.on_swap_quote_flow(
                            &flow.pool_dedupe_key(),
                            side,
                            flow.quote_volume,
                            flow.quote_liquidity,
                            flow.mark_price_ratio,
                            flow.ts_unix,
                        );
                    }
                }
                _ = async {
                    if let Some(interval) = &mut tracking_tick {
                        interval.tick().await;
                    }
                }, if tracking_tick.is_some() => {
                    if let Some(tracker) = self.tracking.as_mut() {
                        tracker.on_tick().await;
                    }
                }
            }
        }

        info!("event pipeline stopped");
    }
}

fn build_tracking_seed(
    event: &VenueEvent,
    token_meta: Option<&TokenMetadataLite>,
    rugcheck: Option<&RugCheckReport>,
    top_holders: &[crate::domain::HolderShare],
) -> PoolTrackingSeed {
    let token_symbol = token_meta
        .and_then(|m| m.symbol.clone())
        .or_else(|| rugcheck.and_then(|r| r.file_meta.symbol.clone()))
        .or_else(|| rugcheck.and_then(|r| r.token_meta.symbol.clone()))
        .unwrap_or_else(|| symbol_for_mint(&event.token_mint));
    let token_name = token_meta
        .and_then(|m| m.name.clone())
        .or_else(|| rugcheck.and_then(|r| r.file_meta.name.clone()))
        .or_else(|| rugcheck.and_then(|r| r.token_meta.name.clone()))
        .unwrap_or_else(|| token_symbol.clone());
    let quote_symbol = symbol_for_mint(&event.quote_mint);

    PoolTrackingSeed {
        event: event.clone(),
        detected_at_unix: now_unix_secs(),
        token_name,
        token_symbol,
        quote_symbol,
        top_holders: top_holders.to_vec(),
        holder_aggregates: HolderAggregates::from_holders(top_holders),
        rug: rugcheck
            .map(rug_snapshot_from_report)
            .unwrap_or_else(RugSnapshot::default),
    }
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
