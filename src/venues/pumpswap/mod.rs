use crate::constants::MAX_SEEN_SIGNATURES;
use crate::domain::{FlowSide, PoolSwapFlowEvent, VenueEvent, VenueId};
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
use std::collections::{HashMap, HashSet, VecDeque};
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tracing::{error, info, warn};

const CREATE_POOL_IX_DISCRIMINATOR: [u8; 8] = [233, 146, 209, 142, 207, 104, 64, 188];
const BUY_IX_DISCRIMINATOR: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
const SELL_IX_DISCRIMINATOR: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];
const CREATE_POOL_EVENT_DISCRIMINATOR: [u8; 8] = [177, 49, 12, 210, 160, 118, 167, 116];
const CREATE_POOL_EVENT_MIN_BYTES_TO_POOL: usize = 197;

#[derive(Clone, Debug)]
struct PumpSwapPoolCreate {
    pool_id: String,
    base_mint: String,
    quote_mint: String,
    event_seen: bool,
}

#[derive(Clone, Debug)]
struct PumpSwapCreatePoolEvent {
    pool_id: String,
    base_mint: String,
    quote_mint: String,
}

#[derive(Clone, Debug)]
struct PumpSwapTrackedPool {
    token_mint: String,
    quote_mint: String,
}

pub struct PumpSwapWatcher {
    program_id: Pubkey,
    allowed_quote_mints: HashSet<String>,
    venue: VenueId,
}

impl PumpSwapWatcher {
    pub fn new(program_id: Pubkey, allowed_quote_mints: HashSet<String>) -> Self {
        Self {
            program_id,
            allowed_quote_mints,
            venue: VenueId::new("pumpswap", "amm", "PumpSwap"),
        }
    }

    async fn run(self, runtime: VenueRuntime) -> Result<()> {
        let ps_client = PubsubClient::new(&runtime.ws_url)
            .await
            .context("failed to connect ws for pumpswap logs listener")?;
        let program_id = self.program_id.to_string();

        let (mut stream, _unsub) = ps_client
            .logs_subscribe(
                RpcTransactionLogsFilter::Mentions(vec![program_id.clone()]),
                RpcTransactionLogsConfig {
                    commitment: Some(CommitmentConfig::confirmed()),
                },
            )
            .await
            .context("logs_subscribe failed for pumpswap program")?;

        let mut seen_signatures: HashSet<String> = HashSet::new();
        let mut seen_order: VecDeque<String> = VecDeque::new();
        let mut seen_pool_ids: HashSet<String> = HashSet::new();
        let mut tracked_pools: HashMap<String, PumpSwapTrackedPool> = HashMap::new();

        info!(
            venue = %self.venue.slug(),
            program_id = %self.program_id,
            allowed_quote_mints = ?self.allowed_quote_mints,
            create_pool_ix_discriminator = ?CREATE_POOL_IX_DISCRIMINATOR,
            create_pool_event_discriminator = ?CREATE_POOL_EVENT_DISCRIMINATOR,
            "pumpswap create_pool listener subscribed"
        );

        while let Some(msg) = stream.next().await {
            if msg.value.err.is_some() {
                continue;
            }
            let has_create_pool = log_has_create_pool(&msg.value.logs);
            let has_swap = log_has_swap(&msg.value.logs);
            if !has_create_pool && !has_swap {
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

            let Some(tx_json) = fetch_pumpswap_transaction_json_with_retry(
                &runtime.http_client,
                &runtime.rpc_url,
                &signature,
                &program_id,
            )
            .await
            .map_err(|err| {
                warn!(
                    venue = %self.venue.slug(),
                    signature = %signature,
                    ?err,
                    "failed to resolve pumpswap transaction"
                );
                err
            })
            .ok()
            .flatten() else {
                continue;
            };

            if has_create_pool
                && let Some(pool_create) =
                    extract_pumpswap_pool_create_from_get_transaction_json(&tx_json, &program_id)
            {
                if let Some((token_mint, quote_mint)) = select_token_and_quote(
                    &pool_create.base_mint,
                    &pool_create.quote_mint,
                    &self.allowed_quote_mints,
                ) {
                    tracked_pools.entry(pool_create.pool_id.clone()).or_insert(
                        PumpSwapTrackedPool {
                            token_mint: token_mint.clone(),
                            quote_mint: quote_mint.clone(),
                        },
                    );

                    if seen_pool_ids.insert(pool_create.pool_id.clone()) {
                        if runtime
                            .event_tx
                            .send(VenueEvent {
                                venue: self.venue.clone(),
                                signature: signature.clone(),
                                pool_id: pool_create.pool_id.clone(),
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
                            pool_id = %pool_create.pool_id,
                            base_mint = %pool_create.base_mint,
                            quote_mint = %pool_create.quote_mint,
                            event_seen = pool_create.event_seen,
                            "decoded pumpswap create_pool"
                        );
                    }
                } else {
                    info!(
                        venue = %self.venue.slug(),
                        signature = %signature,
                        pool_id = %pool_create.pool_id,
                        base_mint = %pool_create.base_mint,
                        quote_mint = %pool_create.quote_mint,
                        "create_pool detected but pair does not match quote filter"
                    );
                }
            }

            if has_swap
                && let Some(flow_event) = extract_pumpswap_swap_flow_from_get_transaction_json(
                    &tx_json,
                    &self.venue,
                    &program_id,
                    &tracked_pools,
                )
                && runtime.flow_tx.send(flow_event).is_err()
            {
                warn!("flow channel closed; stopping watcher");
                break;
            }
        }

        warn!(venue = %self.venue.slug(), "pumpswap logs stream ended");
        Ok(())
    }
}

impl VenueWatcher for PumpSwapWatcher {
    fn name(&self) -> &'static str {
        "pumpswap"
    }

    fn spawn(self: Box<Self>, runtime: VenueRuntime) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(err) = self.run(runtime).await {
                error!(?err, "pumpswap logs listener failed");
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

fn log_has_create_pool(logs: &[String]) -> bool {
    logs.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("instruction: create_pool") || lower.contains("instruction: createpool")
    })
}

fn log_has_swap(logs: &[String]) -> bool {
    logs.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("instruction: buy") || lower.contains("instruction: sell")
    })
}

async fn fetch_pumpswap_transaction_json_with_retry(
    http_client: &HttpClient,
    rpc_url: &str,
    signature: &str,
    _program_id: &str,
) -> Result<Option<Value>> {
    let attempts = 6usize;
    let mut last_err: Option<anyhow::Error> = None;

    for i in 0..attempts {
        match fetch_pumpswap_transaction_json(http_client, rpc_url, signature).await {
            Ok(Some(tx_json)) => return Ok(Some(tx_json)),
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

async fn fetch_pumpswap_transaction_json(
    http_client: &HttpClient,
    rpc_url: &str,
    signature: &str,
) -> Result<Option<Value>> {
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

    Ok(Some(tx_json))
}

fn extract_pumpswap_pool_create_from_get_transaction_json(
    tx_json: &Value,
    program_id: &str,
) -> Option<PumpSwapPoolCreate> {
    let mut top_level = extract_create_pool_from_top_level_instruction(tx_json, program_id)?;
    if let Some(ev) = extract_create_pool_event_from_inner_instructions(tx_json) {
        top_level.pool_id = ev.pool_id;
        top_level.base_mint = ev.base_mint;
        top_level.quote_mint = ev.quote_mint;
        top_level.event_seen = true;
    }
    Some(top_level)
}

fn extract_pumpswap_swap_flow_from_get_transaction_json(
    tx_json: &Value,
    venue: &VenueId,
    program_id: &str,
    tracked_pools: &HashMap<String, PumpSwapTrackedPool>,
) -> Option<PoolSwapFlowEvent> {
    if tracked_pools.is_empty() {
        return None;
    }

    let instructions = message_instructions(tx_json)?;
    let account_keys = message_account_keys(tx_json);
    let (pre_balances, post_balances) = token_balance_maps(tx_json);
    if pre_balances.is_empty() || post_balances.is_empty() {
        return None;
    }

    for ix in instructions {
        if !instruction_targets_program(ix, account_keys, program_id) {
            continue;
        }

        let Some(raw_data) = decode_instruction_data(ix) else {
            continue;
        };
        let is_buy = raw_data.starts_with(&BUY_IX_DISCRIMINATOR);
        let is_sell = raw_data.starts_with(&SELL_IX_DISCRIMINATOR);
        if !is_buy && !is_sell {
            continue;
        }

        let pool_id = ix_account_pubkey(ix, account_keys, 0)?;
        let tracked = tracked_pools.get(&pool_id)?;
        let base_mint = ix_account_pubkey(ix, account_keys, 3)?;
        let quote_mint = ix_account_pubkey(ix, account_keys, 4)?;
        let base_vault_idx = ix_account_index(ix, account_keys, 7)?;
        let quote_vault_idx = ix_account_index(ix, account_keys, 8)?;

        let (token_vault_idx, quote_vault_idx, token_is_base) =
            if tracked.token_mint == base_mint && tracked.quote_mint == quote_mint {
                (base_vault_idx, quote_vault_idx, true)
            } else if tracked.token_mint == quote_mint && tracked.quote_mint == base_mint {
                (quote_vault_idx, base_vault_idx, false)
            } else {
                continue;
            };

        let quote_pre = pre_balances.get(&quote_vault_idx).copied()?;
        let quote_post = post_balances.get(&quote_vault_idx).copied()?;
        let quote_volume = (quote_post - quote_pre).abs();
        if !quote_volume.is_finite() || quote_volume <= 0.0 {
            continue;
        }

        let token_pre = pre_balances.get(&token_vault_idx).copied();
        let token_post = post_balances.get(&token_vault_idx).copied();
        let side = match (token_pre, token_post) {
            (Some(pre), Some(post)) if post < pre => FlowSide::Buy,
            (Some(pre), Some(post)) if post > pre => FlowSide::Sell,
            _ if is_buy && token_is_base => FlowSide::Buy,
            _ if is_buy && !token_is_base => FlowSide::Sell,
            _ if is_sell && token_is_base => FlowSide::Sell,
            _ => FlowSide::Buy,
        };

        let quote_liquidity = Some(quote_post);
        let mark_price_ratio = match (token_post, quote_liquidity) {
            (Some(token_liq), Some(quote_liq)) if token_liq > 0.0 => Some(quote_liq / token_liq),
            _ => None,
        };

        let ts_unix = tx_json
            .pointer("/result/blockTime")
            .and_then(Value::as_i64)
            .and_then(|v| if v >= 0 { Some(v as u64) } else { None })
            .unwrap_or_else(now_unix_secs);

        return Some(PoolSwapFlowEvent {
            venue: venue.clone(),
            pool_id,
            side,
            quote_volume,
            quote_liquidity,
            mark_price_ratio,
            ts_unix,
        });
    }

    None
}

fn extract_create_pool_from_top_level_instruction(
    tx_json: &Value,
    program_id: &str,
) -> Option<PumpSwapPoolCreate> {
    let instructions = message_instructions(tx_json)?;
    let account_keys = message_account_keys(tx_json);

    for ix in instructions {
        if !instruction_targets_program(ix, account_keys, program_id) {
            continue;
        }

        let Some(raw_data) = decode_instruction_data(ix) else {
            continue;
        };
        if !raw_data.starts_with(&CREATE_POOL_IX_DISCRIMINATOR) {
            continue;
        }

        let pool_id = ix_account_pubkey(ix, account_keys, 0)?;
        let base_mint = ix_account_pubkey(ix, account_keys, 3)?;
        let quote_mint = ix_account_pubkey(ix, account_keys, 4)?;

        return Some(PumpSwapPoolCreate {
            pool_id,
            base_mint,
            quote_mint,
            event_seen: false,
        });
    }

    None
}

fn extract_create_pool_event_from_inner_instructions(
    tx_json: &Value,
) -> Option<PumpSwapCreatePoolEvent> {
    let inner = tx_json
        .pointer("/result/meta/innerInstructions")
        .and_then(Value::as_array)
        .or_else(|| {
            tx_json
                .pointer("/result/transaction/meta/innerInstructions")
                .and_then(Value::as_array)
        })?;

    for group in inner {
        let Some(instructions) = group.get("instructions").and_then(Value::as_array) else {
            continue;
        };

        for ix in instructions {
            let Some(raw) = decode_instruction_data(ix) else {
                continue;
            };
            if let Some(ev) = parse_create_pool_event_from_ix_data(&raw) {
                return Some(ev);
            }
        }
    }

    None
}

fn parse_create_pool_event_from_ix_data(raw: &[u8]) -> Option<PumpSwapCreatePoolEvent> {
    if raw.len() < EVENT_IX_TAG_LE.len() + 8 + CREATE_POOL_EVENT_MIN_BYTES_TO_POOL {
        return None;
    }
    if !raw.starts_with(EVENT_IX_TAG_LE) {
        return None;
    }

    let payload = &raw[EVENT_IX_TAG_LE.len()..];
    if !payload.starts_with(&CREATE_POOL_EVENT_DISCRIMINATOR) {
        return None;
    }

    let mut offset = CREATE_POOL_EVENT_DISCRIMINATOR.len();

    let _timestamp = parse_i64(payload, &mut offset)?;
    let _index = parse_u16(payload, &mut offset)?;
    let _creator = parse_pubkey(payload, &mut offset)?;
    let base_mint = parse_pubkey(payload, &mut offset)?;
    let quote_mint = parse_pubkey(payload, &mut offset)?;
    let _base_decimals = parse_u8(payload, &mut offset)?;
    let _quote_decimals = parse_u8(payload, &mut offset)?;
    let _base_amount_in = parse_u64(payload, &mut offset)?;
    let _quote_amount_in = parse_u64(payload, &mut offset)?;
    let _pool_base_amount = parse_u64(payload, &mut offset)?;
    let _pool_quote_amount = parse_u64(payload, &mut offset)?;
    let _minimum_liquidity = parse_u64(payload, &mut offset)?;
    let _initial_liquidity = parse_u64(payload, &mut offset)?;
    let _lp_token_amount_out = parse_u64(payload, &mut offset)?;
    let _pool_bump = parse_u8(payload, &mut offset)?;
    let pool_id = parse_pubkey(payload, &mut offset)?;

    Some(PumpSwapCreatePoolEvent {
        pool_id,
        base_mint,
        quote_mint,
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

fn ix_account_index(
    ix: &Value,
    account_keys: Option<&Vec<Value>>,
    account_position: usize,
) -> Option<usize> {
    let accounts = ix.get("accounts").and_then(Value::as_array)?;
    let value = accounts.get(account_position)?;

    if let Some(index) = value.as_u64() {
        return Some(index as usize);
    }

    if let Some(pubkey) = value.as_str() {
        return account_keys.and_then(|keys| account_key_index(keys, pubkey));
    }

    value
        .get("pubkey")
        .and_then(Value::as_str)
        .and_then(|pubkey| account_keys.and_then(|keys| account_key_index(keys, pubkey)))
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

fn account_key_index(account_keys: &[Value], pubkey: &str) -> Option<usize> {
    for (idx, entry) in account_keys.iter().enumerate() {
        if entry.as_str() == Some(pubkey)
            || entry.get("pubkey").and_then(Value::as_str) == Some(pubkey)
        {
            return Some(idx);
        }
    }
    None
}

fn token_balance_maps(tx_json: &Value) -> (HashMap<usize, f64>, HashMap<usize, f64>) {
    (
        token_balance_map(
            tx_json
                .pointer("/result/meta/preTokenBalances")
                .and_then(Value::as_array)
                .or_else(|| {
                    tx_json
                        .pointer("/result/transaction/meta/preTokenBalances")
                        .and_then(Value::as_array)
                }),
        ),
        token_balance_map(
            tx_json
                .pointer("/result/meta/postTokenBalances")
                .and_then(Value::as_array)
                .or_else(|| {
                    tx_json
                        .pointer("/result/transaction/meta/postTokenBalances")
                        .and_then(Value::as_array)
                }),
        ),
    )
}

fn token_balance_map(entries: Option<&Vec<Value>>) -> HashMap<usize, f64> {
    let mut out = HashMap::new();
    let Some(entries) = entries else {
        return out;
    };

    for entry in entries {
        let Some(account_index) = entry.get("accountIndex").and_then(Value::as_u64) else {
            continue;
        };
        let Some(amount) = token_balance_ui_amount(entry) else {
            continue;
        };
        out.insert(account_index as usize, amount);
    }
    out
}

fn token_balance_ui_amount(entry: &Value) -> Option<f64> {
    let ui = entry.get("uiTokenAmount")?;
    ui.get("uiAmount").and_then(Value::as_f64).or_else(|| {
        ui.get("uiAmountString")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<f64>().ok())
    })
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

fn parse_u64(data: &[u8], offset: &mut usize) -> Option<u64> {
    if *offset + 8 > data.len() {
        return None;
    }
    let bytes: [u8; 8] = data[*offset..*offset + 8].try_into().ok()?;
    *offset += 8;
    Some(u64::from_le_bytes(bytes))
}

fn parse_i64(data: &[u8], offset: &mut usize) -> Option<i64> {
    if *offset + 8 > data.len() {
        return None;
    }
    let bytes: [u8; 8] = data[*offset..*offset + 8].try_into().ok()?;
    *offset += 8;
    Some(i64::from_le_bytes(bytes))
}

fn parse_pubkey(data: &[u8], offset: &mut usize) -> Option<String> {
    if *offset + 32 > data.len() {
        return None;
    }
    let bytes: [u8; 32] = data[*offset..*offset + 32].try_into().ok()?;
    *offset += 32;
    Some(Pubkey::new_from_array(bytes).to_string())
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::str::FromStr;

    #[test]
    fn decodes_create_pool_instruction_discriminator() {
        let tx_json = json!({
            "result": {
                "transaction": {
                    "message": {
                        "instructions": [
                            {
                                "programId": "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
                                "accounts": [
                                    "E4zaPoqVN3U2jPZGwUBAeYkojp8adLP8vPdhPmxxDRab",
                                    "cfg",
                                    "creator",
                                    "So11111111111111111111111111111111111111112",
                                    "J1Qq59HRddSHWHnM2mJhC4LC5s924MunWN7GXQznjey4"
                                ],
                                "data": bs58::encode(CREATE_POOL_IX_DISCRIMINATOR).into_string()
                            }
                        ]
                    }
                }
            }
        });

        let parsed = extract_create_pool_from_top_level_instruction(
            &tx_json,
            "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
        )
        .unwrap();

        assert_eq!(
            parsed.pool_id,
            "E4zaPoqVN3U2jPZGwUBAeYkojp8adLP8vPdhPmxxDRab".to_string()
        );
        assert_eq!(
            parsed.base_mint,
            "So11111111111111111111111111111111111111112".to_string()
        );
        assert_eq!(
            parsed.quote_mint,
            "J1Qq59HRddSHWHnM2mJhC4LC5s924MunWN7GXQznjey4".to_string()
        );
    }

    #[test]
    fn decodes_create_pool_event_prefix_fields() {
        let pool = Pubkey::from_str("E4zaPoqVN3U2jPZGwUBAeYkojp8adLP8vPdhPmxxDRab").unwrap();
        let creator = Pubkey::from_str("9FocCF6xBtJ48cBHpQS97Uu6mqYL1SnYrLdEJbNsqTdk").unwrap();
        let base_mint = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();
        let quote_mint = Pubkey::from_str("J1Qq59HRddSHWHnM2mJhC4LC5s924MunWN7GXQznjey4").unwrap();

        let mut payload = Vec::new();
        payload.extend_from_slice(&1771686984_i64.to_le_bytes());
        payload.extend_from_slice(&0_u16.to_le_bytes());
        payload.extend_from_slice(creator.as_ref());
        payload.extend_from_slice(base_mint.as_ref());
        payload.extend_from_slice(quote_mint.as_ref());
        payload.extend_from_slice(&9_u8.to_le_bytes());
        payload.extend_from_slice(&6_u8.to_le_bytes());
        payload.extend_from_slice(&50000000000_u64.to_le_bytes());
        payload.extend_from_slice(&1000000000000000_u64.to_le_bytes());
        payload.extend_from_slice(&50000000000_u64.to_le_bytes());
        payload.extend_from_slice(&1000000000000000_u64.to_le_bytes());
        payload.extend_from_slice(&100_u64.to_le_bytes());
        payload.extend_from_slice(&7071067811865_u64.to_le_bytes());
        payload.extend_from_slice(&7071067811765_u64.to_le_bytes());
        payload.extend_from_slice(&253_u8.to_le_bytes());
        payload.extend_from_slice(pool.as_ref());
        payload.extend_from_slice(Pubkey::new_unique().as_ref());
        payload.extend_from_slice(Pubkey::new_unique().as_ref());
        payload.extend_from_slice(Pubkey::new_unique().as_ref());
        payload.extend_from_slice(
            Pubkey::from_str("11111111111111111111111111111111")
                .unwrap()
                .as_ref(),
        );
        payload.extend_from_slice(&0_u8.to_le_bytes());

        let mut raw = Vec::new();
        raw.extend_from_slice(EVENT_IX_TAG_LE);
        raw.extend_from_slice(&CREATE_POOL_EVENT_DISCRIMINATOR);
        raw.extend_from_slice(&payload);

        let parsed = parse_create_pool_event_from_ix_data(&raw).unwrap();
        assert_eq!(parsed.pool_id, pool.to_string());
        assert_eq!(parsed.base_mint, base_mint.to_string());
        assert_eq!(parsed.quote_mint, quote_mint.to_string());
    }
}
