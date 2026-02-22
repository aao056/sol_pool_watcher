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

const INITIALIZE_POOL_IX_DISCRIMINATOR: [u8; 8] = [95, 180, 10, 172, 84, 174, 232, 40];
const EVT_INITIALIZE_POOL_EVENT_DISCRIMINATOR: [u8; 8] = [228, 50, 246, 85, 203, 66, 134, 37];
const INITIALIZE_POOL_ACCOUNT_INDEX_POOL: usize = 6;
const INITIALIZE_POOL_ACCOUNT_INDEX_TOKEN_A_MINT: usize = 8;
const INITIALIZE_POOL_ACCOUNT_INDEX_TOKEN_B_MINT: usize = 9;

#[derive(Clone, Debug)]
struct DammPoolInitialized {
    pool_id: String,
    token_a_mint: String,
    token_b_mint: String,
    creator: Option<String>,
    payer: Option<String>,
    event_seen: bool,
}

#[derive(Clone, Debug)]
struct EvtInitializePool {
    pool_id: String,
    token_a_mint: String,
    token_b_mint: String,
    creator: String,
    payer: String,
}

pub struct MeteoraDammWatcher {
    program_id: Pubkey,
    allowed_quote_mints: HashSet<String>,
    venue: VenueId,
}

impl MeteoraDammWatcher {
    pub fn new(program_id: Pubkey, allowed_quote_mints: HashSet<String>) -> Self {
        Self {
            program_id,
            allowed_quote_mints,
            venue: VenueId::new("meteora", "damm", "Meteora DAMM"),
        }
    }

    async fn run(self, runtime: VenueRuntime) -> Result<()> {
        let ps_client = PubsubClient::new(&runtime.ws_url)
            .await
            .context("failed to connect ws for meteora damm logs listener")?;

        let (mut stream, _unsub) = ps_client
            .logs_subscribe(
                RpcTransactionLogsFilter::Mentions(vec![self.program_id.to_string()]),
                RpcTransactionLogsConfig {
                    commitment: Some(CommitmentConfig::confirmed()),
                },
            )
            .await
            .context("logs_subscribe failed for meteora damm program")?;

        let mut seen_signatures: HashSet<String> = HashSet::new();
        let mut seen_order: VecDeque<String> = VecDeque::new();
        let mut seen_pool_ids: HashSet<String> = HashSet::new();
        let program_id_str = self.program_id.to_string();

        info!(
            venue = %self.venue.slug(),
            program_id = %self.program_id,
            allowed_quote_mints = ?self.allowed_quote_mints,
            initialize_pool_ix_discriminator = ?INITIALIZE_POOL_IX_DISCRIMINATOR,
            evt_initialize_pool_event_discriminator = ?EVT_INITIALIZE_POOL_EVENT_DISCRIMINATOR,
            "meteora damm initialize_pool listener subscribed"
        );

        while let Some(msg) = stream.next().await {
            if msg.value.err.is_some() {
                continue;
            }
            if !log_has_initialize_pool(&msg.value.logs) {
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

            let Some(pool_init) = fetch_damm_pool_initialized_with_retry(
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
                    "failed to resolve meteora damm initialize_pool transaction"
                );
                err
            })
            .ok()
            .flatten() else {
                info!(
                    venue = %self.venue.slug(),
                    signature = %signature,
                    "initialize_pool detected but pool/token fields were not decoded"
                );
                continue;
            };

            if !seen_pool_ids.insert(pool_init.pool_id.clone()) {
                continue;
            }

            let Some((token_mint, quote_mint)) = select_token_and_quote(
                &pool_init.token_a_mint,
                &pool_init.token_b_mint,
                &self.allowed_quote_mints,
            ) else {
                info!(
                    venue = %self.venue.slug(),
                    signature = %signature,
                    pool_id = %pool_init.pool_id,
                    token_a_mint = %pool_init.token_a_mint,
                    token_b_mint = %pool_init.token_b_mint,
                    creator = ?pool_init.creator,
                    payer = ?pool_init.payer,
                    "initialize_pool detected but pair does not match quote filter"
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
                token_a_mint = %pool_init.token_a_mint,
                token_b_mint = %pool_init.token_b_mint,
                creator = ?pool_init.creator,
                payer = ?pool_init.payer,
                event_seen = pool_init.event_seen,
                "decoded meteora damm initialize_pool"
            );
        }

        warn!(venue = %self.venue.slug(), "meteora damm logs stream ended");
        Ok(())
    }
}

impl VenueWatcher for MeteoraDammWatcher {
    fn name(&self) -> &'static str {
        "meteora_damm"
    }

    fn spawn(self: Box<Self>, runtime: VenueRuntime) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(err) = self.run(runtime).await {
                error!(?err, "meteora damm logs listener failed");
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

fn log_has_initialize_pool(logs: &[String]) -> bool {
    logs.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        let is_initialize = lower.contains("instruction: initialize_pool")
            || lower.contains("instruction: initializepool");
        let is_dynamic = lower.contains("instruction: initialize_pool_with_dynamic_config")
            || lower.contains("instruction: initializepoolwithdynamicconfig");
        is_initialize && !is_dynamic
    })
}

async fn fetch_damm_pool_initialized_with_retry(
    http_client: &HttpClient,
    rpc_url: &str,
    signature: &str,
    program_id: &str,
) -> Result<Option<DammPoolInitialized>> {
    let attempts = 6usize;
    let mut last_err: Option<anyhow::Error> = None;

    for i in 0..attempts {
        match fetch_damm_pool_initialized(http_client, rpc_url, signature, program_id).await {
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

async fn fetch_damm_pool_initialized(
    http_client: &HttpClient,
    rpc_url: &str,
    signature: &str,
    program_id: &str,
) -> Result<Option<DammPoolInitialized>> {
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

    Ok(extract_damm_pool_initialized_from_get_transaction_json(
        &tx_json, program_id,
    ))
}

fn extract_damm_pool_initialized_from_get_transaction_json(
    tx_json: &Value,
    program_id: &str,
) -> Option<DammPoolInitialized> {
    let mut top_level = extract_initialize_pool_from_top_level_instruction(tx_json, program_id)?;
    if let Some(ev) = extract_evt_initialize_pool_from_inner_instructions(tx_json, program_id) {
        top_level.pool_id = ev.pool_id;
        top_level.token_a_mint = ev.token_a_mint;
        top_level.token_b_mint = ev.token_b_mint;
        top_level.creator = Some(ev.creator);
        top_level.payer = Some(ev.payer);
        top_level.event_seen = true;
    }
    Some(top_level)
}

fn extract_initialize_pool_from_top_level_instruction(
    tx_json: &Value,
    program_id: &str,
) -> Option<DammPoolInitialized> {
    let instructions = message_instructions(tx_json)?;
    let account_keys = message_account_keys(tx_json);

    for ix in instructions {
        if !instruction_targets_program(ix, account_keys, program_id) {
            continue;
        }

        let Some(raw_data) = decode_instruction_data(ix) else {
            continue;
        };
        if !raw_data.starts_with(&INITIALIZE_POOL_IX_DISCRIMINATOR) {
            continue;
        }

        let pool_id = ix_account_pubkey(ix, account_keys, INITIALIZE_POOL_ACCOUNT_INDEX_POOL)?;
        let token_a_mint =
            ix_account_pubkey(ix, account_keys, INITIALIZE_POOL_ACCOUNT_INDEX_TOKEN_A_MINT)?;
        let token_b_mint =
            ix_account_pubkey(ix, account_keys, INITIALIZE_POOL_ACCOUNT_INDEX_TOKEN_B_MINT)?;

        return Some(DammPoolInitialized {
            pool_id,
            token_a_mint,
            token_b_mint,
            creator: None,
            payer: None,
            event_seen: false,
        });
    }

    None
}

fn extract_evt_initialize_pool_from_inner_instructions(
    tx_json: &Value,
    program_id: &str,
) -> Option<EvtInitializePool> {
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
            if let Some(ev) = parse_evt_initialize_pool_from_ix_data(&raw) {
                return Some(ev);
            }
        }
    }

    None
}

fn parse_evt_initialize_pool_from_ix_data(raw: &[u8]) -> Option<EvtInitializePool> {
    const EVENT_PREFIX_LEN: usize = EVENT_IX_TAG_LE.len() + 8 + 32 * 6;
    if raw.len() < EVENT_PREFIX_LEN {
        return None;
    }
    if !raw.starts_with(EVENT_IX_TAG_LE) {
        return None;
    }

    let payload = &raw[EVENT_IX_TAG_LE.len()..];
    if !payload.starts_with(&EVT_INITIALIZE_POOL_EVENT_DISCRIMINATOR) {
        return None;
    }

    let mut offset = EVT_INITIALIZE_POOL_EVENT_DISCRIMINATOR.len();
    let pool_id = parse_pubkey(payload, &mut offset)?;
    let token_a_mint = parse_pubkey(payload, &mut offset)?;
    let token_b_mint = parse_pubkey(payload, &mut offset)?;
    let creator = parse_pubkey(payload, &mut offset)?;
    let payer = parse_pubkey(payload, &mut offset)?;
    let _alpha_vault = parse_pubkey(payload, &mut offset)?;

    Some(EvtInitializePool {
        pool_id,
        token_a_mint,
        token_b_mint,
        creator,
        payer,
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
    fn decodes_initialize_pool_instruction_discriminator() {
        let tx_json = json!({
            "result": {
                "transaction": {
                    "message": {
                        "instructions": [
                            {
                                "programId": "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG",
                                "accounts": [
                                    "creator",
                                    "position_nft_mint",
                                    "position_nft_account",
                                    "payer",
                                    "config",
                                    "HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC",
                                    "7iKMXDTFDhAh9fMizCpZVDDVj5ifb38ZojjGUb5ChmPK",
                                    "position",
                                    "2fy3QceaT4KEjQh1RKxiTzd2q9yBdXTFoebzgyPyx7VH",
                                    "4btUt5tQrN9MSfiHzZMdHf2ognjQZrWErjSJ9aiApump"
                                ],
                                "data": bs58::encode(INITIALIZE_POOL_IX_DISCRIMINATOR).into_string()
                            }
                        ]
                    }
                }
            }
        });

        let parsed = extract_initialize_pool_from_top_level_instruction(
            &tx_json,
            "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG",
        )
        .unwrap();

        assert_eq!(
            parsed.pool_id,
            "7iKMXDTFDhAh9fMizCpZVDDVj5ifb38ZojjGUb5ChmPK".to_string()
        );
        assert_eq!(
            parsed.token_a_mint,
            "2fy3QceaT4KEjQh1RKxiTzd2q9yBdXTFoebzgyPyx7VH".to_string()
        );
        assert_eq!(
            parsed.token_b_mint,
            "4btUt5tQrN9MSfiHzZMdHf2ognjQZrWErjSJ9aiApump".to_string()
        );
        assert!(!parsed.event_seen);
    }

    #[test]
    fn decodes_evt_initialize_pool_prefix_fields() {
        let pool = Pubkey::from_str("7iKMXDTFDhAh9fMizCpZVDDVj5ifb38ZojjGUb5ChmPK").unwrap();
        let token_a = Pubkey::from_str("2fy3QceaT4KEjQh1RKxiTzd2q9yBdXTFoebzgyPyx7VH").unwrap();
        let token_b = Pubkey::from_str("4btUt5tQrN9MSfiHzZMdHf2ognjQZrWErjSJ9aiApump").unwrap();
        let creator = Pubkey::from_str("78hTPBbppfVhV9bXuWY8x82bRy2DfeEfRxhmKiXRa6bg").unwrap();
        let payer = Pubkey::from_str("78hTPBbppfVhV9bXuWY8x82bRy2DfeEfRxhmKiXRa6bg").unwrap();
        let alpha_vault = Pubkey::from_str("11111111111111111111111111111111").unwrap();

        let mut raw = Vec::new();
        raw.extend_from_slice(EVENT_IX_TAG_LE);
        raw.extend_from_slice(&EVT_INITIALIZE_POOL_EVENT_DISCRIMINATOR);
        raw.extend_from_slice(pool.as_ref());
        raw.extend_from_slice(token_a.as_ref());
        raw.extend_from_slice(token_b.as_ref());
        raw.extend_from_slice(creator.as_ref());
        raw.extend_from_slice(payer.as_ref());
        raw.extend_from_slice(alpha_vault.as_ref());

        let parsed = parse_evt_initialize_pool_from_ix_data(&raw).unwrap();
        assert_eq!(parsed.pool_id, pool.to_string());
        assert_eq!(parsed.token_a_mint, token_a.to_string());
        assert_eq!(parsed.token_b_mint, token_b.to_string());
        assert_eq!(parsed.creator, creator.to_string());
        assert_eq!(parsed.payer, payer.to_string());
    }

    #[test]
    fn extracts_evt_initialize_pool_from_inner_instruction_shape() {
        let pool = Pubkey::from_str("7iKMXDTFDhAh9fMizCpZVDDVj5ifb38ZojjGUb5ChmPK").unwrap();
        let token_a = Pubkey::from_str("2fy3QceaT4KEjQh1RKxiTzd2q9yBdXTFoebzgyPyx7VH").unwrap();
        let token_b = Pubkey::from_str("4btUt5tQrN9MSfiHzZMdHf2ognjQZrWErjSJ9aiApump").unwrap();
        let creator = Pubkey::from_str("78hTPBbppfVhV9bXuWY8x82bRy2DfeEfRxhmKiXRa6bg").unwrap();
        let payer = Pubkey::from_str("78hTPBbppfVhV9bXuWY8x82bRy2DfeEfRxhmKiXRa6bg").unwrap();
        let alpha_vault = Pubkey::from_str("11111111111111111111111111111111").unwrap();

        let mut event_ix_data = Vec::new();
        event_ix_data.extend_from_slice(EVENT_IX_TAG_LE);
        event_ix_data.extend_from_slice(&EVT_INITIALIZE_POOL_EVENT_DISCRIMINATOR);
        event_ix_data.extend_from_slice(pool.as_ref());
        event_ix_data.extend_from_slice(token_a.as_ref());
        event_ix_data.extend_from_slice(token_b.as_ref());
        event_ix_data.extend_from_slice(creator.as_ref());
        event_ix_data.extend_from_slice(payer.as_ref());
        event_ix_data.extend_from_slice(alpha_vault.as_ref());

        let tx_json = json!({
            "result": {
                "transaction": {
                    "message": {
                        "accountKeys": [
                            {"pubkey": "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG"}
                        ]
                    }
                },
                "meta": {
                    "innerInstructions": [
                        {
                            "instructions": [
                                {
                                    "programId": "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG",
                                    "data": bs58::encode(event_ix_data).into_string()
                                }
                            ]
                        }
                    ]
                }
            }
        });

        let parsed = extract_evt_initialize_pool_from_inner_instructions(
            &tx_json,
            "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG",
        )
        .unwrap();
        assert_eq!(parsed.pool_id, pool.to_string());
        assert_eq!(parsed.token_a_mint, token_a.to_string());
        assert_eq!(parsed.token_b_mint, token_b.to_string());
        assert_eq!(parsed.creator, creator.to_string());
        assert_eq!(parsed.payer, payer.to_string());
    }
}
