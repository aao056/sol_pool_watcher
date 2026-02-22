use crate::constants::MAX_SEEN_SIGNATURES;
use crate::domain::{VenueEvent, VenueId};
use crate::services::select_token_and_quote;
use crate::venues::{VenueRuntime, VenueWatcher};
use anchor_lang::{AccountDeserialize, AnchorDeserialize, Discriminator};
use anyhow::{Context, Result};
use base64::Engine as _;
use futures_util::StreamExt;
use raydium_amm_v3::states::{PoolCreatedEvent, PoolState};
use reqwest::Client as HttpClient;
use serde_json::{Value, json};
use solana_pubkey::Pubkey;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_types::config::{
    CommitmentConfig, RpcTransactionLogsConfig, RpcTransactionLogsFilter,
};
use std::collections::{HashSet, VecDeque};
use std::str::FromStr;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

pub struct RaydiumClmmWatcher {
    program_id: Pubkey,
    allowed_quote_mints: HashSet<String>,
    venue: VenueId,
}

impl RaydiumClmmWatcher {
    pub fn new(program_id: Pubkey, allowed_quote_mints: HashSet<String>) -> Self {
        Self {
            program_id,
            allowed_quote_mints,
            venue: VenueId::new("raydium", "clmm", "Raydium CLMM"),
        }
    }

    async fn run(self, runtime: VenueRuntime) -> Result<()> {
        let ps_client = PubsubClient::new(&runtime.ws_url)
            .await
            .context("failed to connect ws for clmm create_pool logs listener")?;

        let (mut stream, _unsub) = ps_client
            .logs_subscribe(
                RpcTransactionLogsFilter::Mentions(vec![self.program_id.to_string()]),
                RpcTransactionLogsConfig {
                    commitment: Some(CommitmentConfig::confirmed()),
                },
            )
            .await
            .context("logs_subscribe failed for clmm program")?;

        let mut seen_signatures: HashSet<String> = HashSet::new();
        let mut seen_order: VecDeque<String> = VecDeque::new();
        let mut seen_pool_ids: HashSet<String> = HashSet::new();
        let clmm_program_id_str = self.program_id.to_string();

        info!(
            venue = %self.venue.slug(),
            program_id = %self.program_id,
            allowed_quote_mints = ?self.allowed_quote_mints,
            "clmm create_pool instruction listener subscribed"
        );

        while let Some(msg) = stream.next().await {
            if msg.value.err.is_some() {
                continue;
            }
            if !log_has_create_pool(&msg.value.logs) {
                continue;
            }

            let signature = msg.value.signature;
            if !remember_signature(
                &mut seen_signatures,
                &mut seen_order,
                &signature,
                MAX_SEEN_SIGNATURES,
            ) {
                continue;
            }

            let decoded_event = extract_pool_created_event_from_logs(&msg.value.logs);

            let (pool_id, mint0, mint1) = if let Some(ev) = decoded_event {
                (
                    ev.pool_state.to_string(),
                    ev.token_mint_0.to_string(),
                    ev.token_mint_1.to_string(),
                )
            } else {
                let pool_id_opt = fetch_pool_id_from_signature(
                    &runtime.http_client,
                    &runtime.rpc_url,
                    &signature,
                    &clmm_program_id_str,
                )
                .await
                .unwrap_or(None);

                let Some(pool_id) = pool_id_opt else {
                    info!(
                        venue = %self.venue.slug(),
                        program_id = %self.program_id,
                        signature = %signature,
                        "create_pool detected but pool_id unresolved"
                    );
                    continue;
                };

                let pool_pubkey = match Pubkey::from_str(&pool_id) {
                    Ok(pk) => pk,
                    Err(err) => {
                        warn!(
                            venue = %self.venue.slug(),
                            pool_id = %pool_id,
                            ?err,
                            "invalid pool_id from create_pool tx"
                        );
                        continue;
                    }
                };

                let (mint0, mint1) =
                    match fetch_clmm_pool_mints(runtime.rpc_client.as_ref(), &pool_pubkey).await {
                        Ok(v) => v,
                        Err(err) => {
                            warn!(
                                venue = %self.venue.slug(),
                                pool_id = %pool_id,
                                ?err,
                                "failed to fetch/decode CLMM pool mints"
                            );
                            continue;
                        }
                    };

                (pool_id, mint0, mint1)
            };

            if !seen_pool_ids.insert(pool_id.clone()) {
                continue;
            }

            let Some((token_mint, quote_mint)) =
                select_token_and_quote(&mint0, &mint1, &self.allowed_quote_mints)
            else {
                info!(
                    venue = %self.venue.slug(),
                    pool_id = %pool_id,
                    mint0 = %mint0,
                    mint1 = %mint1,
                    "create_pool detected but pair does not match quote filter"
                );
                continue;
            };

            if runtime
                .event_tx
                .send(VenueEvent {
                    venue: self.venue.clone(),
                    signature,
                    pool_id,
                    token_mint,
                    quote_mint,
                })
                .is_err()
            {
                warn!("event channel closed; stopping watcher");
                break;
            }
        }

        warn!(venue = %self.venue.slug(), "clmm create_pool logs stream ended");
        Ok(())
    }
}

impl VenueWatcher for RaydiumClmmWatcher {
    fn name(&self) -> &'static str {
        "raydium_clmm"
    }

    fn spawn(self: Box<Self>, runtime: VenueRuntime) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(err) = self.run(runtime).await {
                error!(?err, "clmm create_pool logs listener failed");
            }
        })
    }
}

fn deserialize_anchor_from_bytes<T: AccountDeserialize>(bytes: &[u8]) -> Result<T> {
    let mut slice = bytes;
    Ok(T::try_deserialize(&mut slice)?)
}

fn remember_signature(
    seen: &mut HashSet<String>,
    order: &mut VecDeque<String>,
    signature: &str,
    cap: usize,
) -> bool {
    if !seen.insert(signature.to_string()) {
        return false;
    }
    order.push_back(signature.to_string());
    while order.len() > cap {
        if let Some(evicted) = order.pop_front() {
            seen.remove(&evicted);
        }
    }
    true
}

fn log_has_create_pool(logs: &[String]) -> bool {
    logs.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("instruction: createpool") || lower.contains("instruction: create_pool")
    })
}

fn decode_pool_created_event_from_log_line(line: &str) -> Option<PoolCreatedEvent> {
    let encoded = line
        .strip_prefix("Program data: ")
        .or_else(|| line.strip_prefix("Program log: "))?;

    let raw = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    if raw.len() < 8 {
        return None;
    }

    let mut disc = [0u8; 8];
    disc.copy_from_slice(&raw[0..8]);
    if disc != PoolCreatedEvent::DISCRIMINATOR {
        return None;
    }

    let mut slice: &[u8] = &raw[8..];
    PoolCreatedEvent::deserialize(&mut slice).ok()
}

fn extract_pool_created_event_from_logs(logs: &[String]) -> Option<PoolCreatedEvent> {
    for line in logs {
        if let Some(ev) = decode_pool_created_event_from_log_line(line) {
            return Some(ev);
        }
    }
    None
}

fn extract_pool_id_from_instruction_json(
    ix: &Value,
    account_keys: Option<&Vec<Value>>,
    program_id_str: &str,
) -> Option<String> {
    if let Some(ix_program) = ix.get("programId").and_then(Value::as_str)
        && ix_program == program_id_str
    {
        return ix
            .get("accounts")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(Value::as_str)
            .map(ToString::to_string);
    }

    let account_keys = account_keys?;
    let program_idx = ix.get("programIdIndex").and_then(Value::as_u64)? as usize;
    let account_idx_arr = ix.get("accounts").and_then(Value::as_array)?;
    let first_acc_idx = account_idx_arr.first().and_then(Value::as_u64)? as usize;

    let key_at = |idx: usize| -> Option<String> {
        let entry = account_keys.get(idx)?;
        if let Some(s) = entry.as_str() {
            return Some(s.to_string());
        }
        entry
            .get("pubkey")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    };

    let ix_program_id = key_at(program_idx)?;
    if ix_program_id != program_id_str {
        return None;
    }
    key_at(first_acc_idx)
}

fn extract_pool_id_from_get_transaction_json(
    tx_json: &Value,
    clmm_program_id: &str,
) -> Option<String> {
    let message = tx_json.pointer("/result/transaction/transaction/message")?;
    let instructions = message.get("instructions").and_then(Value::as_array)?;
    let account_keys = message.get("accountKeys").and_then(Value::as_array);

    for ix in instructions {
        if let Some(pool_id) =
            extract_pool_id_from_instruction_json(ix, account_keys, clmm_program_id)
        {
            return Some(pool_id);
        }
    }
    None
}

async fn fetch_pool_id_from_signature(
    http_client: &HttpClient,
    rpc_url: &str,
    signature: &str,
    clmm_program_id: &str,
) -> Result<Option<String>> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTransaction",
        "params": [
            signature,
            {
                "encoding": "jsonParsed",
                "commitment": "confirmed",
                "maxSupportedTransactionVersion": 0
            }
        ]
    });

    let response = http_client
        .post(rpc_url)
        .json(&body)
        .send()
        .await
        .context("getTransaction HTTP request failed")?;
    let status = response.status();
    let tx_json: Value = response
        .json()
        .await
        .context("failed to decode getTransaction response json")?;

    if !status.is_success() {
        return Ok(None);
    }
    if tx_json.get("error").is_some() {
        return Ok(None);
    }
    Ok(extract_pool_id_from_get_transaction_json(
        &tx_json,
        clmm_program_id,
    ))
}

async fn fetch_clmm_pool_mints(rpc: &RpcClient, pool_id: &Pubkey) -> Result<(String, String)> {
    let account = rpc
        .get_account(pool_id)
        .await
        .with_context(|| format!("failed to fetch pool account {pool_id}"))?;

    let pool_state: PoolState =
        deserialize_anchor_from_bytes(&account.data).context("failed to decode PoolState")?;

    let mint0 = Pubkey::new_from_array(pool_state.token_mint_0.to_bytes()).to_string();
    let mint1 = Pubkey::new_from_array(pool_state.token_mint_1.to_bytes()).to_string();
    Ok((mint0, mint1))
}
