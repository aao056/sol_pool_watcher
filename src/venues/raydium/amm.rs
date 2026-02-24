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

const INITIALIZE2_ACCOUNTS_LEN: usize = 21;
const INITIALIZE2_ACCOUNT_INDEX_AMM: usize = 4;
const INITIALIZE2_ACCOUNT_INDEX_COIN_MINT: usize = 8;
const INITIALIZE2_ACCOUNT_INDEX_PC_MINT: usize = 9;

#[derive(Clone, Debug)]
struct RaydiumAmmInitialize2 {
    pool_id: String,
    coin_mint: String,
    pc_mint: String,
}

pub struct RaydiumAmmWatcher {
    program_id: Pubkey,
    allowed_quote_mints: HashSet<String>,
    venue: VenueId,
}

impl RaydiumAmmWatcher {
    pub fn new(program_id: Pubkey, allowed_quote_mints: HashSet<String>) -> Self {
        Self {
            program_id,
            allowed_quote_mints,
            venue: VenueId::new("raydium", "amm", "Raydium AMM"),
        }
    }

    async fn run(self, runtime: VenueRuntime) -> Result<()> {
        let ps_client = PubsubClient::new(&runtime.ws_url)
            .await
            .context("failed to connect ws for raydium amm logs listener")?;

        let (mut stream, _unsub) = ps_client
            .logs_subscribe(
                RpcTransactionLogsFilter::Mentions(vec![self.program_id.to_string()]),
                RpcTransactionLogsConfig {
                    commitment: Some(CommitmentConfig::confirmed()),
                },
            )
            .await
            .context("logs_subscribe failed for raydium amm program")?;

        let mut seen_signatures: HashSet<String> = HashSet::new();
        let mut seen_order: VecDeque<String> = VecDeque::new();
        let mut seen_pool_ids: HashSet<String> = HashSet::new();
        let program_id_str = self.program_id.to_string();

        info!(
            venue = %self.venue.slug(),
            program_id = %self.program_id,
            allowed_quote_mints = ?self.allowed_quote_mints,
            "raydium amm initialize2 listener subscribed"
        );

        while let Some(msg) = stream.next().await {
            if msg.value.err.is_some() {
                continue;
            }
            if !log_has_initialize2(&msg.value.logs) {
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

            let Some(init) = fetch_raydium_amm_initialize2_with_retry(
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
                    "failed to resolve raydium amm initialize2 transaction"
                );
                err
            })
            .ok()
            .flatten() else {
                info!(
                    venue = %self.venue.slug(),
                    signature = %signature,
                    "initialize2 detected but failed to decode pool/coin/pc mints"
                );
                continue;
            };

            if !seen_pool_ids.insert(init.pool_id.clone()) {
                continue;
            }

            let Some((token_mint, quote_mint)) =
                select_token_and_quote(&init.coin_mint, &init.pc_mint, &self.allowed_quote_mints)
            else {
                info!(
                    venue = %self.venue.slug(),
                    signature = %signature,
                    pool_id = %init.pool_id,
                    coin_mint = %init.coin_mint,
                    pc_mint = %init.pc_mint,
                    "initialize2 detected but pair does not match quote filter"
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
                coin_mint = %init.coin_mint,
                pc_mint = %init.pc_mint,
                "decoded raydium amm initialize2"
            );
        }

        warn!(venue = %self.venue.slug(), "raydium amm logs stream ended");
        Ok(())
    }
}

impl VenueWatcher for RaydiumAmmWatcher {
    fn name(&self) -> &'static str {
        "raydium_amm"
    }

    fn spawn(self: Box<Self>, runtime: VenueRuntime) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(err) = self.run(runtime).await {
                error!(?err, "raydium amm logs listener failed");
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

fn log_has_initialize2(logs: &[String]) -> bool {
    logs.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("instruction: initialize2")
            || lower.contains("instruction: initialize_2")
            || lower.contains("initialize2:")
    })
}

async fn fetch_raydium_amm_initialize2_with_retry(
    http_client: &HttpClient,
    rpc_url: &str,
    signature: &str,
    program_id: &str,
) -> Result<Option<RaydiumAmmInitialize2>> {
    let attempts = 6usize;
    let mut last_err: Option<anyhow::Error> = None;

    for i in 0..attempts {
        match fetch_raydium_amm_initialize2(http_client, rpc_url, signature, program_id).await {
            Ok(Some(pool)) => return Ok(Some(pool)),
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

async fn fetch_raydium_amm_initialize2(
    http_client: &HttpClient,
    rpc_url: &str,
    signature: &str,
    program_id: &str,
) -> Result<Option<RaydiumAmmInitialize2>> {
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

    Ok(extract_raydium_amm_initialize2_from_get_transaction_json(
        &tx_json, program_id,
    ))
}

fn extract_raydium_amm_initialize2_from_get_transaction_json(
    tx_json: &Value,
    program_id: &str,
) -> Option<RaydiumAmmInitialize2> {
    let instructions = message_instructions(tx_json)?;
    let account_keys = message_account_keys(tx_json);

    for ix in instructions {
        if !instruction_targets_program(ix, account_keys, program_id) {
            continue;
        }
        if ix_accounts_len(ix)? < INITIALIZE2_ACCOUNTS_LEN {
            continue;
        }

        // Raydium AMM v4 IDL initialize2 layout is 21 accounts. Data isn't Anchor-discriminated.
        let Some(raw_data) = decode_instruction_data(ix) else {
            continue;
        };
        if raw_data.len() < 25 {
            continue;
        }

        let pool_id = ix_account_pubkey(ix, account_keys, INITIALIZE2_ACCOUNT_INDEX_AMM)?;
        let coin_mint = ix_account_pubkey(ix, account_keys, INITIALIZE2_ACCOUNT_INDEX_COIN_MINT)?;
        let pc_mint = ix_account_pubkey(ix, account_keys, INITIALIZE2_ACCOUNT_INDEX_PC_MINT)?;

        return Some(RaydiumAmmInitialize2 {
            pool_id,
            coin_mint,
            pc_mint,
        });
    }

    None
}

fn ix_accounts_len(ix: &Value) -> Option<usize> {
    ix.get("accounts").and_then(Value::as_array).map(Vec::len)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const RAY_AMM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

    #[test]
    fn detects_initialize2_log_variants() {
        let logs = vec![
            "Program log: initialize2: InitializeInstruction2 { ... }".to_string(),
            "Program log: ray_log: abc".to_string(),
        ];
        assert!(log_has_initialize2(&logs));

        let no_init_logs = vec!["Program log: ray_log: abc".to_string()];
        assert!(!log_has_initialize2(&no_init_logs));
    }

    #[test]
    fn extracts_initialize2_from_top_level_instruction() {
        let mut accounts = Vec::new();
        for i in 0..INITIALIZE2_ACCOUNTS_LEN {
            accounts.push(format!("Account{i}111111111111111111111111111111111"));
        }
        accounts[INITIALIZE2_ACCOUNT_INDEX_AMM] =
            "Pool111111111111111111111111111111111111111".to_string();
        accounts[INITIALIZE2_ACCOUNT_INDEX_COIN_MINT] =
            "Coin111111111111111111111111111111111111111".to_string();
        accounts[INITIALIZE2_ACCOUNT_INDEX_PC_MINT] =
            "So11111111111111111111111111111111111111112".to_string();

        let tx_json = json!({
            "result": {
                "transaction": {
                    "message": {
                        "instructions": [
                            {
                                "programId": RAY_AMM,
                                "accounts": accounts,
                                "data": bs58::encode(vec![9u8; 26]).into_string()
                            }
                        ]
                    }
                }
            }
        });

        let parsed = extract_raydium_amm_initialize2_from_get_transaction_json(&tx_json, RAY_AMM)
            .expect("expected initialize2 parse");

        assert_eq!(
            parsed.pool_id,
            "Pool111111111111111111111111111111111111111"
        );
        assert_eq!(
            parsed.coin_mint,
            "Coin111111111111111111111111111111111111111"
        );
        assert_eq!(
            parsed.pc_mint,
            "So11111111111111111111111111111111111111112"
        );
    }

    #[test]
    fn extracts_initialize2_from_compiled_instruction_form() {
        let tx_json = json!({
            "result": {
                "transaction": {
                    "message": {
                        "accountKeys": [
                            "Sys111111111111111111111111111111111111111",
                            RAY_AMM,
                            "Pool111111111111111111111111111111111111111",
                            "AA1",
                            "AA2",
                            "AA3",
                            "AA4",
                            "AA5",
                            "Coin111111111111111111111111111111111111111",
                            "So11111111111111111111111111111111111111112",
                            "AA8","AA9","AA10","AA11","AA12","AA13","AA14","AA15","AA16","AA17","AA18","AA19","AA20"
                        ],
                        "instructions": [
                            {
                                "programIdIndex": 1,
                                "accounts": [0,0,0,0,2,0,0,0,8,9,0,0,0,0,0,0,0,0,0,0,0],
                                "data": [ "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=", "base64" ]
                            }
                        ]
                    }
                }
            }
        });

        let parsed = extract_raydium_amm_initialize2_from_get_transaction_json(&tx_json, RAY_AMM)
            .expect("expected compiled initialize2 parse");

        assert_eq!(
            parsed.pool_id,
            "Pool111111111111111111111111111111111111111"
        );
        assert_eq!(
            parsed.coin_mint,
            "Coin111111111111111111111111111111111111111"
        );
        assert_eq!(
            parsed.pc_mint,
            "So11111111111111111111111111111111111111112"
        );
    }
}
