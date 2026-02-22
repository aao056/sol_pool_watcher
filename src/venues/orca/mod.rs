use crate::constants::MAX_SEEN_SIGNATURES;
use crate::domain::{VenueEvent, VenueId};
use crate::services::select_token_and_quote;
use crate::venues::{VenueRuntime, VenueWatcher};
use anchor_lang::event::EVENT_IX_TAG_LE;
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

const INITIALIZE_POOL_V2_IX_DISCRIMINATOR: [u8; 8] = [207, 45, 87, 242, 27, 63, 204, 67];
const POOL_INITIALIZED_EVENT_DISCRIMINATOR: [u8; 8] = [100, 118, 173, 87, 12, 198, 254, 229];
const INITIALIZE_POOL_V2_ACCOUNT_INDEX_TOKEN_MINT_A: usize = 1;
const INITIALIZE_POOL_V2_ACCOUNT_INDEX_TOKEN_MINT_B: usize = 2;
const INITIALIZE_POOL_V2_ACCOUNT_INDEX_WHIRLPOOL: usize = 6;

#[derive(Clone, Debug)]
struct OrcaPoolInitialized {
    pool_id: String,
    token_mint_a: String,
    token_mint_b: String,
    tick_spacing: Option<u16>,
    event_seen: bool,
}

#[derive(Clone, Debug)]
struct PoolInitializedEvent {
    pool_id: String,
    token_mint_a: String,
    token_mint_b: String,
    tick_spacing: u16,
}

pub struct OrcaWhirlpoolWatcher {
    program_id: Pubkey,
    allowed_quote_mints: HashSet<String>,
    venue: VenueId,
}

impl OrcaWhirlpoolWatcher {
    pub fn new(program_id: Pubkey, allowed_quote_mints: HashSet<String>) -> Self {
        Self {
            program_id,
            allowed_quote_mints,
            venue: VenueId::new("orca", "whirlpool", "Orca Whirlpool"),
        }
    }

    async fn run(self, runtime: VenueRuntime) -> Result<()> {
        let ps_client = PubsubClient::new(&runtime.ws_url)
            .await
            .context("failed to connect ws for orca whirlpool logs listener")?;

        let (mut stream, _unsub) = ps_client
            .logs_subscribe(
                RpcTransactionLogsFilter::Mentions(vec![self.program_id.to_string()]),
                RpcTransactionLogsConfig {
                    commitment: Some(CommitmentConfig::confirmed()),
                },
            )
            .await
            .context("logs_subscribe failed for orca whirlpool program")?;

        let mut seen_signatures: HashSet<String> = HashSet::new();
        let mut seen_order: VecDeque<String> = VecDeque::new();
        let mut seen_pool_ids: HashSet<String> = HashSet::new();
        let program_id_str = self.program_id.to_string();

        info!(
            venue = %self.venue.slug(),
            program_id = %self.program_id,
            allowed_quote_mints = ?self.allowed_quote_mints,
            initialize_pool_v2_ix_discriminator = ?INITIALIZE_POOL_V2_IX_DISCRIMINATOR,
            pool_initialized_event_discriminator = ?POOL_INITIALIZED_EVENT_DISCRIMINATOR,
            "orca initialize_pool_v2 listener subscribed"
        );

        while let Some(msg) = stream.next().await {
            if msg.value.err.is_some() {
                continue;
            }
            if !log_has_initialize_pool_v2(&msg.value.logs) {
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

            let Some(pool_init) = fetch_orca_pool_initialized_with_retry(
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
                    "failed to resolve orca initialize_pool_v2 transaction"
                );
                err
            })
            .ok()
            .flatten() else {
                info!(
                    venue = %self.venue.slug(),
                    signature = %signature,
                    "initialize_pool_v2 detected but whirlpool event was not decoded"
                );
                continue;
            };

            if !seen_pool_ids.insert(pool_init.pool_id.clone()) {
                continue;
            }

            let Some((token_mint, quote_mint)) = select_token_and_quote(
                &pool_init.token_mint_a,
                &pool_init.token_mint_b,
                &self.allowed_quote_mints,
            ) else {
                info!(
                    venue = %self.venue.slug(),
                    signature = %signature,
                    pool_id = %pool_init.pool_id,
                    token_mint_a = %pool_init.token_mint_a,
                    token_mint_b = %pool_init.token_mint_b,
                    tick_spacing = ?pool_init.tick_spacing,
                    "initialize_pool_v2 detected but pair does not match quote filter"
                );
                continue;
            };

            if runtime
                .event_tx
                .send(VenueEvent {
                    venue: self.venue.clone(),
                    signature,
                    pool_id: pool_init.pool_id.clone(),
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
                pool_id = %pool_init.pool_id,
                token_mint_a = %pool_init.token_mint_a,
                token_mint_b = %pool_init.token_mint_b,
                tick_spacing = ?pool_init.tick_spacing,
                event_seen = pool_init.event_seen,
                "decoded orca initialize_pool_v2"
            );
        }

        warn!(venue = %self.venue.slug(), "orca whirlpool logs stream ended");
        Ok(())
    }
}

impl VenueWatcher for OrcaWhirlpoolWatcher {
    fn name(&self) -> &'static str {
        "orca_whirlpool"
    }

    fn spawn(self: Box<Self>, runtime: VenueRuntime) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(err) = self.run(runtime).await {
                error!(?err, "orca whirlpool logs listener failed");
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

fn log_has_initialize_pool_v2(logs: &[String]) -> bool {
    logs.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("instruction: initialize_pool_v2")
            || lower.contains("instruction: initializepoolv2")
    })
}

async fn fetch_orca_pool_initialized_with_retry(
    http_client: &HttpClient,
    rpc_url: &str,
    signature: &str,
    program_id: &str,
) -> Result<Option<OrcaPoolInitialized>> {
    let attempts = 6usize;
    let mut last_err: Option<anyhow::Error> = None;

    for i in 0..attempts {
        match fetch_orca_pool_initialized(http_client, rpc_url, signature, program_id).await {
            Ok(Some(event)) => return Ok(Some(event)),
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

async fn fetch_orca_pool_initialized(
    http_client: &HttpClient,
    rpc_url: &str,
    signature: &str,
    program_id: &str,
) -> Result<Option<OrcaPoolInitialized>> {
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
    if tx_json.get("result").is_none() || tx_json.get("result").is_some_and(Value::is_null) {
        return Ok(None);
    }

    Ok(extract_orca_pool_initialized_from_get_transaction_json(
        &tx_json, program_id,
    ))
}

fn extract_orca_pool_initialized_from_get_transaction_json(
    tx_json: &Value,
    program_id: &str,
) -> Option<OrcaPoolInitialized> {
    let mut top_level = extract_initialize_pool_v2_from_top_level_instruction(tx_json, program_id)?;
    if let Some(ev) = extract_pool_initialized_event_from_inner_instructions(tx_json, program_id) {
        top_level.pool_id = ev.pool_id;
        top_level.token_mint_a = ev.token_mint_a;
        top_level.token_mint_b = ev.token_mint_b;
        top_level.tick_spacing = Some(ev.tick_spacing);
        top_level.event_seen = true;
    }
    Some(top_level)
}

fn extract_initialize_pool_v2_from_top_level_instruction(
    tx_json: &Value,
    program_id: &str,
) -> Option<OrcaPoolInitialized> {
    let instructions = message_instructions(tx_json)?;
    let account_keys = message_account_keys(tx_json);

    for ix in instructions {
        if !instruction_targets_program(ix, account_keys, program_id) {
            continue;
        }

        let Some(raw_data) = decode_instruction_data(ix) else {
            continue;
        };
        if !raw_data.starts_with(&INITIALIZE_POOL_V2_IX_DISCRIMINATOR) {
            continue;
        }

        let pool_id =
            ix_account_pubkey(ix, account_keys, INITIALIZE_POOL_V2_ACCOUNT_INDEX_WHIRLPOOL)?;
        let token_mint_a = ix_account_pubkey(
            ix,
            account_keys,
            INITIALIZE_POOL_V2_ACCOUNT_INDEX_TOKEN_MINT_A,
        )?;
        let token_mint_b = ix_account_pubkey(
            ix,
            account_keys,
            INITIALIZE_POOL_V2_ACCOUNT_INDEX_TOKEN_MINT_B,
        )?;

        return Some(OrcaPoolInitialized {
            pool_id,
            token_mint_a,
            token_mint_b,
            tick_spacing: None,
            event_seen: false,
        });
    }

    None
}

fn extract_pool_initialized_event_from_inner_instructions(
    tx_json: &Value,
    program_id: &str,
) -> Option<PoolInitializedEvent> {
    let inner = tx_json
        .pointer("/result/meta/innerInstructions")
        .and_then(Value::as_array)
        .or_else(|| {
            tx_json
                .pointer("/result/transaction/meta/innerInstructions")
                .and_then(Value::as_array)
        })?;
    let account_keys = message_account_keys(tx_json);

    for group in inner {
        let Some(instructions) = group.get("instructions").and_then(Value::as_array) else {
            continue;
        };

        for ix in instructions {
            if !instruction_targets_program(ix, account_keys, program_id) {
                continue;
            }

            let Some(raw) = decode_instruction_data(ix) else {
                continue;
            };
            if let Some(ev) = parse_pool_initialized_event_from_ix_data(&raw) {
                return Some(ev);
            }
        }
    }

    None
}

fn parse_pool_initialized_event_from_ix_data(raw: &[u8]) -> Option<PoolInitializedEvent> {
    if raw.len() < EVENT_IX_TAG_LE.len() + 8 + 32 + 32 + 32 + 32 + 2 + 32 + 32 + 1 + 1 + 16 {
        return None;
    }
    if !raw.starts_with(EVENT_IX_TAG_LE) {
        return None;
    }

    let payload = &raw[EVENT_IX_TAG_LE.len()..];
    if !payload.starts_with(&POOL_INITIALIZED_EVENT_DISCRIMINATOR) {
        return None;
    }

    let mut offset = POOL_INITIALIZED_EVENT_DISCRIMINATOR.len();
    let pool_id = parse_pubkey(payload, &mut offset)?;
    let _whirlpools_config = parse_pubkey(payload, &mut offset)?;
    let token_mint_a = parse_pubkey(payload, &mut offset)?;
    let token_mint_b = parse_pubkey(payload, &mut offset)?;
    let tick_spacing = parse_u16(payload, &mut offset)?;
    let _token_program_a = parse_pubkey(payload, &mut offset)?;
    let _token_program_b = parse_pubkey(payload, &mut offset)?;
    let _decimals_a = parse_u8(payload, &mut offset)?;
    let _decimals_b = parse_u8(payload, &mut offset)?;
    let _initial_sqrt_price = parse_u128(payload, &mut offset)?;

    Some(PoolInitializedEvent {
        pool_id,
        token_mint_a,
        token_mint_b,
        tick_spacing,
    })
}

fn message_instructions(tx_json: &Value) -> Option<&Vec<Value>> {
    tx_json
        .pointer("/result/transaction/message/instructions")
        .and_then(Value::as_array)
        .or_else(|| {
            tx_json
                .pointer("/result/transaction/transaction/message/instructions")
                .and_then(Value::as_array)
        })
}

fn message_account_keys(tx_json: &Value) -> Option<&Vec<Value>> {
    tx_json
        .pointer("/result/transaction/message/accountKeys")
        .and_then(Value::as_array)
        .or_else(|| {
            tx_json
                .pointer("/result/transaction/transaction/message/accountKeys")
                .and_then(Value::as_array)
        })
}

fn instruction_targets_program(
    ix: &Value,
    account_keys: Option<&Vec<Value>>,
    expected_program_id: &str,
) -> bool {
    if let Some(ix_program) = ix.get("programId").and_then(Value::as_str) {
        return ix_program == expected_program_id;
    }

    let Some(account_keys) = account_keys else {
        return false;
    };
    let Some(program_idx) = ix.get("programIdIndex").and_then(Value::as_u64) else {
        return false;
    };

    account_key_at(account_keys, program_idx as usize).as_deref() == Some(expected_program_id)
}

fn ix_account_pubkey(
    ix: &Value,
    account_keys: Option<&Vec<Value>>,
    account_position: usize,
) -> Option<String> {
    let accounts = ix.get("accounts").and_then(Value::as_array)?;
    let value = accounts.get(account_position)?;

    if let Some(pk) = value.as_str() {
        return Some(pk.to_string());
    }
    if let Some(index) = value.as_u64() {
        let keys = account_keys?;
        return account_key_at(keys, index as usize);
    }
    value
        .get("pubkey")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn account_key_at(account_keys: &[Value], idx: usize) -> Option<String> {
    let entry = account_keys.get(idx)?;
    if let Some(s) = entry.as_str() {
        return Some(s.to_string());
    }
    entry
        .get("pubkey")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn decode_instruction_data(ix: &Value) -> Option<Vec<u8>> {
    if let Some(data_str) = ix.get("data").and_then(Value::as_str) {
        return bs58::decode(data_str).into_vec().ok();
    }

    let data_arr = ix.get("data").and_then(Value::as_array)?;
    if data_arr.len() != 2 {
        return None;
    }
    let payload = data_arr.first().and_then(Value::as_str)?;
    let encoding = data_arr.get(1).and_then(Value::as_str)?;

    match encoding {
        "base58" => bs58::decode(payload).into_vec().ok(),
        "base64" => {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(payload)
                .ok()
        }
        _ => None,
    }
}

fn parse_u8(data: &[u8], offset: &mut usize) -> Option<u8> {
    if *offset + 1 > data.len() {
        return None;
    }
    let v = data[*offset];
    *offset += 1;
    Some(v)
}

fn parse_u16(data: &[u8], offset: &mut usize) -> Option<u16> {
    if *offset + 2 > data.len() {
        return None;
    }
    let bytes: [u8; 2] = data[*offset..*offset + 2].try_into().ok()?;
    *offset += 2;
    Some(u16::from_le_bytes(bytes))
}

fn parse_u128(data: &[u8], offset: &mut usize) -> Option<u128> {
    if *offset + 16 > data.len() {
        return None;
    }
    let bytes: [u8; 16] = data[*offset..*offset + 16].try_into().ok()?;
    *offset += 16;
    Some(u128::from_le_bytes(bytes))
}

fn parse_pubkey(data: &[u8], offset: &mut usize) -> Option<String> {
    if *offset + 32 > data.len() {
        return None;
    }
    let bytes: [u8; 32] = data[*offset..*offset + 32].try_into().ok()?;
    *offset += 32;
    Some(Pubkey::new_from_array(bytes).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::str::FromStr;

    #[test]
    fn decodes_initialize_pool_v2_instruction_discriminator() {
        let tx_json = json!({
            "result": {
                "transaction": {
                    "message": {
                        "instructions": [
                            {
                                "programId": "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc",
                                "accounts": [
                                    "2LecshUwdy9xi7meFgHtFJQNSKk4KdTrcpvaB56dP2NQ",
                                    "25wQxTrpyV2FXUrZMG7KrooJyLfor5PAxyV14K2pA89B",
                                    "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                                    "badge-a",
                                    "badge-b",
                                    "funder",
                                    "8ucpJbnMXJe2va5uVWB99R5upVi6GDrepUffWdjXc4rR"
                                ],
                                "data": bs58::encode(INITIALIZE_POOL_V2_IX_DISCRIMINATOR).into_string()
                            }
                        ]
                    }
                }
            }
        });

        let parsed = extract_initialize_pool_v2_from_top_level_instruction(
            &tx_json,
            "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc",
        )
        .unwrap();

        assert_eq!(
            parsed.pool_id,
            "8ucpJbnMXJe2va5uVWB99R5upVi6GDrepUffWdjXc4rR".to_string()
        );
        assert_eq!(
            parsed.token_mint_a,
            "25wQxTrpyV2FXUrZMG7KrooJyLfor5PAxyV14K2pA89B".to_string()
        );
        assert_eq!(
            parsed.token_mint_b,
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".to_string()
        );
        assert!(!parsed.event_seen);
        assert!(parsed.tick_spacing.is_none());
    }

    #[test]
    fn decodes_pool_initialized_event_prefix_fields() {
        let whirlpool = Pubkey::from_str("8ucpJbnMXJe2va5uVWB99R5upVi6GDrepUffWdjXc4rR").unwrap();
        let config = Pubkey::from_str("2LecshUwdy9xi7meFgHtFJQNSKk4KdTrcpvaB56dP2NQ").unwrap();
        let mint_a = Pubkey::from_str("25wQxTrpyV2FXUrZMG7KrooJyLfor5PAxyV14K2pA89B").unwrap();
        let mint_b = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
        let token_program =
            Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap();

        let mut payload = Vec::new();
        payload.extend_from_slice(whirlpool.as_ref());
        payload.extend_from_slice(config.as_ref());
        payload.extend_from_slice(mint_a.as_ref());
        payload.extend_from_slice(mint_b.as_ref());
        payload.extend_from_slice(&64u16.to_le_bytes());
        payload.extend_from_slice(token_program.as_ref());
        payload.extend_from_slice(token_program.as_ref());
        payload.push(6u8);
        payload.push(6u8);
        payload.extend_from_slice(&2133707916960598528u128.to_le_bytes());

        let mut raw = Vec::new();
        raw.extend_from_slice(EVENT_IX_TAG_LE);
        raw.extend_from_slice(&POOL_INITIALIZED_EVENT_DISCRIMINATOR);
        raw.extend_from_slice(&payload);

        let parsed = parse_pool_initialized_event_from_ix_data(&raw).unwrap();
        assert_eq!(parsed.pool_id, whirlpool.to_string());
        assert_eq!(parsed.token_mint_a, mint_a.to_string());
        assert_eq!(parsed.token_mint_b, mint_b.to_string());
        assert_eq!(parsed.tick_spacing, 64);
    }

    #[test]
    fn extracts_pool_initialized_event_from_inner_instruction_shape() {
        let whirlpool = Pubkey::from_str("8ucpJbnMXJe2va5uVWB99R5upVi6GDrepUffWdjXc4rR").unwrap();
        let config = Pubkey::from_str("2LecshUwdy9xi7meFgHtFJQNSKk4KdTrcpvaB56dP2NQ").unwrap();
        let mint_a = Pubkey::from_str("25wQxTrpyV2FXUrZMG7KrooJyLfor5PAxyV14K2pA89B").unwrap();
        let mint_b = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
        let token_program =
            Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap();

        let mut event_ix_data = Vec::new();
        event_ix_data.extend_from_slice(EVENT_IX_TAG_LE);
        event_ix_data.extend_from_slice(&POOL_INITIALIZED_EVENT_DISCRIMINATOR);
        event_ix_data.extend_from_slice(whirlpool.as_ref());
        event_ix_data.extend_from_slice(config.as_ref());
        event_ix_data.extend_from_slice(mint_a.as_ref());
        event_ix_data.extend_from_slice(mint_b.as_ref());
        event_ix_data.extend_from_slice(&64u16.to_le_bytes());
        event_ix_data.extend_from_slice(token_program.as_ref());
        event_ix_data.extend_from_slice(token_program.as_ref());
        event_ix_data.push(6u8);
        event_ix_data.push(6u8);
        event_ix_data.extend_from_slice(&2133707916960598528u128.to_le_bytes());

        let tx_json = json!({
            "result": {
                "transaction": {
                    "message": {
                        "accountKeys": [
                            {"pubkey": "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc"}
                        ]
                    }
                },
                "meta": {
                    "innerInstructions": [
                        {
                            "instructions": [
                                {
                                    "programId": "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc",
                                    "data": bs58::encode(event_ix_data).into_string()
                                }
                            ]
                        }
                    ]
                }
            }
        });

        let parsed = extract_pool_initialized_event_from_inner_instructions(
            &tx_json,
            "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc",
        )
        .unwrap();
        assert_eq!(parsed.pool_id, whirlpool.to_string());
        assert_eq!(parsed.token_mint_a, mint_a.to_string());
        assert_eq!(parsed.token_mint_b, mint_b.to_string());
        assert_eq!(parsed.tick_spacing, 64);
    }
}
