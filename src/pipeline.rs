use crate::constants::MAX_SEEN_EVENTS;
use crate::domain::{VenueEvent, symbol_for_mint};
use crate::notifier::TelegramNotifier;
use crate::services::analytics::fetch_top_holders;
use crate::services::message::build_telegram_message;
use crate::services::metadata::fetch_token_metadata;
use crate::services::rugcheck::{fetch_rugcheck_report, holders_from_rugcheck};
use reqwest::Client as HttpClient;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedReceiver;
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
}

impl EventPipeline {
    pub async fn run(self, mut rx: UnboundedReceiver<VenueEvent>) {
        let mut seen_events: HashSet<String> = HashSet::new();
        let mut seen_order: VecDeque<String> = VecDeque::new();

        while let Some(event) = rx.recv().await {
            if !remember_key(
                &mut seen_events,
                &mut seen_order,
                &event.dedupe_key(),
                MAX_SEEN_EVENTS,
            ) {
                continue;
            }

            let token_meta_fut = fetch_token_metadata(self.rpc_client.as_ref(), &event.token_mint);
            let rugcheck_fut = fetch_rugcheck_report(&self.http_client, &event.token_mint);
            let (token_meta, rugcheck) = tokio::join!(token_meta_fut, rugcheck_fut);

            let mut top_holders = rugcheck
                .as_ref()
                .map(|report| holders_from_rugcheck(report, 5))
                .unwrap_or_default();
            if top_holders.is_empty() {
                top_holders =
                    fetch_top_holders(&self.http_client, &self.rpc_url, &event.token_mint, 5)
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

            if let Some(notifier) = &self.telegram {
                let message = build_telegram_message(
                    &event,
                    token_meta.as_ref(),
                    &top_holders,
                    rugcheck.as_ref(),
                );
                notifier.send_event(&message).await;
            }
        }

        info!("event pipeline stopped");
    }
}
