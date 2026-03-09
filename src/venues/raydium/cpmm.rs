use crate::constants::MAX_SEEN_SIGNATURES;
use crate::domain::{VenueEvent, VenueId};
use crate::services::select_token_and_quote;
use crate::venues::{VenueRuntime, VenueWatcher};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use reqwest::Client as HttpClient;
use serde_json::{Value, json};
use solana_pubkey::Pubkey;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client_types::config::{
    CommitmentConfig, RpcTransactionLogsConfig, RpcTransactionLogsFilter,
};
use std::collections::{HashSet, VecDeque};
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tracing::{error, info, warn};

const INITIALIZE_IX_DISCRIMINATOR: [u8; 8] = [175, 175, 109, 31, 13, 152, 155, 237];
const INITIALIZE_ACCOUNT_INDEX_POOL_STATE: usize = 3;
const INITIALIZE_ACCOUNT_INDEX_TOKEN_0_MINT: usize = 4;
const INITIALIZE_ACCOUNT_INDEX_TOKEN_1_MINT: usize = 5;

#[derive(Clone, Debug)]
struct RaydiumCpmmInitialize {
    pool_id: String,
    token_0_mint: String,
    token_1_mint: String,
}

pub struct RaydiumCpmmWatcher {
    program_id: Pubkey,
    allowed_quote_mints: HashSet<String>,
    venue: VenueId,
}

impl RaydiumCpmmWatcher {
    pub fn new(program_id: Pubkey, allowed_quote_mints: HashSet<String>) -> Self {
        Self {
            program_id,
            allowed_quote_mints,
            venue: VenueId::new("raydium", "cpmm", "Raydium CPMM"),
        }
    }

    async fn run(self, runtime: VenueRuntime) -> Result<()> {
        let ps_client = PubsubClient::new(&runtime.ws_url)
            .await
            .context("failed to connect ws for raydium cpmm logs listener")?;

        let (mut stream, _unsub) = ps_client
            .logs_subscribe(
                RpcTransactionLogsFilter::Mentions(vec![self.program_id.to_string()]),
                RpcTransactionLogsConfig {
                    commitment: Some(CommitmentConfig::confirmed()),
                },
            )
            .await
            .context("logs_subscribe failed for raydium cpmm program")?;

        let mut seen_signatures: HashSet<String> = HashSet::new();
        let mut seen_order: VecDeque<String> = VecDeque::new();
        let mut seen_pool_ids: HashSet<String> = HashSet::new();
        let program_id_str = self.program_id.to_string();

        info!(
            venue = %self.venue.slug(),
            program_id = %self.program_id,
            allowed_quote_mints = ?self.allowed_quote_mints,
            initialize_ix_discriminator = ?INITIALIZE_IX_DISCRIMINATOR,
            "raydium cpmm initialize listener subscribed"
        );

        while let Some(msg) = stream.next().await {
            if msg.value.err.is_some() {
                continue;
            }
            if !log_has_initialize(&msg.value.logs) {
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

            let Some(init) = fetch_raydium_cpmm_initialize_with_retry(
                &runtime.http_client,
                &runtime.rpc_url,
                &signature,
                &program_id_str,
            )
            .await
            .map_err(|err| {
                warn!(
                    venue = %self.venue.slug(),
                    signature = %signature,
                    ?err,
                    "failed to resolve raydium cpmm initialize transaction"
                );
                err
            })
            .ok()
            .flatten() else {
                info!(
                    venue = %self.venue.slug(),
                    signature = %signature,
                    "initialize detected but failed to decode pool/token mints"
                );
                continue;
            };

            if !seen_pool_ids.insert(init.pool_id.clone()) {
                continue;
            }

            let Some((token_mint, quote_mint)) = select_token_and_quote(
                &init.token_0_mint,
                &init.token_1_mint,
                &self.allowed_quote_mints,
            ) else {
                info!(
                    venue = %self.venue.slug(),
                    signature = %signature,
                    pool_id = %init.pool_id,
                    token_0_mint = %init.token_0_mint,
                    token_1_mint = %init.token_1_mint,
                    "initialize detected but pair does not match quote filter"
                );
                continue;
            };

            if runtime
                .event_tx
                .send(VenueEvent {
                    venue: self.venue.clone(),
                    signature,
                    pool_id: init.pool_id.clone(),
                    token_mint,
                    quote_mint,
                })
                .is_err()
            {
                warn!("event channel closed; stopping watcher");
                break;
            }

            info!(
                venue = %self.venue.slug(),
                pool_id = %init.pool_id,
                token_0_mint = %init.token_0_mint,
                token_1_mint = %init.token_1_mint,
                "decoded raydium cpmm initialize"
            );
        }

        warn!(venue = %self.venue.slug(), "raydium cpmm logs stream ended");
        Ok(())
    }
}

impl VenueWatcher for RaydiumCpmmWatcher {
    fn name(&self) -> &'static str {
        "raydium_cpmm"
    }

    fn spawn(self: Box<Self>, runtime: VenueRuntime) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(err) = self.run(runtime).await {
                error!(?err, "raydium cpmm logs listener failed");
            }
        })
    }
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

fn log_has_initialize(logs: &[String]) -> bool {
    logs.iter().any(|line| {
        line.trim()
            .eq_ignore_ascii_case("Program log: Instruction: Initialize")
    })
}

async fn fetch_raydium_cpmm_initialize_with_retry(
    http_client: &HttpClient,
    rpc_url: &str,
    signature: &str,
    program_id: &str,
) -> Result<Option<RaydiumCpmmInitialize>> {
    let attempts = 6usize;
    let mut last_err: Option<anyhow::Error> = None;

    for i in 0..attempts {
        match fetch_raydium_cpmm_initialize(http_client, rpc_url, signature, program_id).await {
            Ok(Some(v)) => return Ok(Some(v)),
            Ok(None) => {}
            Err(err) => last_err = Some(err),
        }
        if i + 1 < attempts {
            tokio::time::sleep(Duration::from_millis(850)).await;
        }
    }

    if let Some(err) = last_err {
        return Err(err);
    }
    Ok(None)
}

async fn fetch_raydium_cpmm_initialize(
    http_client: &HttpClient,
    rpc_url: &str,
    signature: &str,
    program_id: &str,
) -> Result<Option<RaydiumCpmmInitialize>> {
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

    if !status.is_success() || tx_json.get("error").is_some() {
        return Ok(None);
    }
    if tx_json.get("result").is_none() || tx_json.get("result").is_some_and(Value::is_null) {
        return Ok(None);
    }

    Ok(extract_cpmm_initialize_from_get_transaction_json(
        &tx_json, program_id,
    ))
}

fn extract_cpmm_initialize_from_get_transaction_json(
    tx_json: &Value,
    program_id: &str,
) -> Option<RaydiumCpmmInitialize> {
    let instructions = message_instructions(tx_json)?;
    let account_keys = message_account_keys(tx_json);

    for ix in instructions {
        if !instruction_targets_program(ix, account_keys, program_id) {
            continue;
        }
        if !instruction_is_initialize(ix) {
            continue;
        }

        let pool_id = parsed_info_pubkey(ix, &["poolState", "pool_state"])
            .or_else(|| ix_account_pubkey(ix, account_keys, INITIALIZE_ACCOUNT_INDEX_POOL_STATE))?;
        let token_0_mint =
            parsed_info_pubkey(ix, &["token0Mint", "token_0_mint"]).or_else(|| {
                ix_account_pubkey(ix, account_keys, INITIALIZE_ACCOUNT_INDEX_TOKEN_0_MINT)
            })?;
        let token_1_mint =
            parsed_info_pubkey(ix, &["token1Mint", "token_1_mint"]).or_else(|| {
                ix_account_pubkey(ix, account_keys, INITIALIZE_ACCOUNT_INDEX_TOKEN_1_MINT)
            })?;

        return Some(RaydiumCpmmInitialize {
            pool_id,
            token_0_mint,
            token_1_mint,
        });
    }

    None
}

fn instruction_is_initialize(ix: &Value) -> bool {
    if let Some(raw_data) = decode_instruction_data(ix)
        && raw_data.starts_with(&INITIALIZE_IX_DISCRIMINATOR)
    {
        return true;
    }

    ix.get("parsed")
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .map(|s| s.eq_ignore_ascii_case("initialize"))
        .unwrap_or(false)
}

fn message_instructions(tx_json: &Value) -> Option<&Vec<Value>> {
    tx_json
        .pointer("/result/transaction/transaction/message/instructions")
        .and_then(Value::as_array)
}

fn message_account_keys(tx_json: &Value) -> Option<&Vec<Value>> {
    tx_json
        .pointer("/result/transaction/transaction/message/accountKeys")
        .and_then(Value::as_array)
}

fn instruction_targets_program(
    ix: &Value,
    account_keys: Option<&Vec<Value>>,
    program_id: &str,
) -> bool {
    if let Some(ix_program) = ix.get("programId").and_then(Value::as_str) {
        return ix_program == program_id;
    }

    let Some(keys) = account_keys else {
        return false;
    };
    let Some(program_idx) = ix.get("programIdIndex").and_then(Value::as_u64) else {
        return false;
    };
    key_at(keys, program_idx as usize).as_deref() == Some(program_id)
}

fn decode_instruction_data(ix: &Value) -> Option<Vec<u8>> {
    let data = ix.get("data")?.as_str()?;
    bs58::decode(data).into_vec().ok()
}

fn parsed_info_pubkey(ix: &Value, keys: &[&str]) -> Option<String> {
    let info = ix.get("parsed")?.get("info")?;
    for key in keys {
        if let Some(v) = info.get(*key).and_then(Value::as_str) {
            return Some(v.to_string());
        }
    }
    None
}

fn ix_account_pubkey(
    ix: &Value,
    account_keys: Option<&Vec<Value>>,
    account_pos: usize,
) -> Option<String> {
    if let Some(accounts) = ix.get("accounts").and_then(Value::as_array)
        && let Some(entry) = accounts.get(account_pos)
    {
        if let Some(s) = entry.as_str() {
            return Some(s.to_string());
        }
        if let Some(idx) = entry.as_u64() {
            let keys = account_keys?;
            return key_at(keys, idx as usize);
        }
        return entry
            .get("pubkey")
            .and_then(Value::as_str)
            .map(ToString::to_string);
    }

    None
}

fn key_at(keys: &[Value], idx: usize) -> Option<String> {
    let entry = keys.get(idx)?;
    if let Some(s) = entry.as_str() {
        return Some(s.to_string());
    }
    entry
        .get("pubkey")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_initialize_from_parsed_instruction_shape() {
        let tx = json!({
            "result": {
                "transaction": {
                    "transaction": {
                        "message": {
                            "accountKeys": [],
                            "instructions": [
                                {
                                    "programId": "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C",
                                    "parsed": {
                                        "type": "initialize",
                                        "info": {
                                            "poolState": "Pool111111111111111111111111111111111111111",
                                            "token0Mint": "Mint011111111111111111111111111111111111111",
                                            "token1Mint": "Mint111111111111111111111111111111111111111"
                                        }
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        });

        let out = extract_cpmm_initialize_from_get_transaction_json(
            &tx,
            "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C",
        )
        .expect("decoded");

        assert_eq!(out.pool_id, "Pool111111111111111111111111111111111111111");
        assert_eq!(
            out.token_0_mint,
            "Mint011111111111111111111111111111111111111"
        );
        assert_eq!(
            out.token_1_mint,
            "Mint111111111111111111111111111111111111111"
        );
    }
}
