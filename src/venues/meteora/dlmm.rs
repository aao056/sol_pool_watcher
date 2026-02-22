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

const INITIALIZE_LB_PAIR2_IX_DISCRIMINATOR: [u8; 8] = [73, 59, 36, 120, 237, 83, 108, 198];
const LB_PAIR_CREATE_EVENT_DISCRIMINATOR: [u8; 8] = [185, 74, 252, 125, 27, 215, 188, 111];

#[derive(Clone, Debug)]
struct LbPairCreateEvent {
    lb_pair: String,
    bin_step: u16,
    token_x: String,
    token_y: String,
}

pub struct MeteoraDlmmWatcher {
    program_id: Pubkey,
    allowed_quote_mints: HashSet<String>,
    venue: VenueId,
}

impl MeteoraDlmmWatcher {
    pub fn new(program_id: Pubkey, allowed_quote_mints: HashSet<String>) -> Self {
        Self {
            program_id,
            allowed_quote_mints,
            venue: VenueId::new("meteora", "dlmm", "Meteora DLMM"),
        }
    }

    async fn run(self, runtime: VenueRuntime) -> Result<()> {
        let ps_client = PubsubClient::new(&runtime.ws_url)
            .await
            .context("failed to connect ws for meteora dlmm logs listener")?;

        let (mut stream, _unsub) = ps_client
            .logs_subscribe(
                RpcTransactionLogsFilter::Mentions(vec![self.program_id.to_string()]),
                RpcTransactionLogsConfig {
                    commitment: Some(CommitmentConfig::confirmed()),
                },
            )
            .await
            .context("logs_subscribe failed for meteora dlmm program")?;

        let mut seen_signatures: HashSet<String> = HashSet::new();
        let mut seen_order: VecDeque<String> = VecDeque::new();
        let mut seen_pool_ids: HashSet<String> = HashSet::new();

        info!(
            venue = %self.venue.slug(),
            program_id = %self.program_id,
            allowed_quote_mints = ?self.allowed_quote_mints,
            initialize_lb_pair2_ix_discriminator = ?INITIALIZE_LB_PAIR2_IX_DISCRIMINATOR,
            lb_pair_create_event_discriminator = ?LB_PAIR_CREATE_EVENT_DISCRIMINATOR,
            "meteora dlmm initialize_lb_pair2 listener subscribed"
        );

        while let Some(msg) = stream.next().await {
            if msg.value.err.is_some() {
                continue;
            }
            if !log_has_initialize_lb_pair2(&msg.value.logs) {
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

            let lb_pair_event = match fetch_lb_pair_create_from_signature_with_retry(
                &runtime.http_client,
                &runtime.rpc_url,
                &signature,
            )
            .await
            {
                Ok(Some(ev)) => ev,
                Ok(None) => {
                    info!(
                        venue = %self.venue.slug(),
                        signature = %signature,
                        "initialize_lb_pair2 detected but lbPairCreate event was not found in inner instructions"
                    );
                    continue;
                }
                Err(err) => {
                    warn!(
                        venue = %self.venue.slug(),
                        signature = %signature,
                        ?err,
                        "failed to resolve lbPairCreate event from transaction"
                    );
                    continue;
                }
            };

            if !seen_pool_ids.insert(lb_pair_event.lb_pair.clone()) {
                continue;
            }

            let Some((token_mint, quote_mint)) = select_token_and_quote(
                &lb_pair_event.token_x,
                &lb_pair_event.token_y,
                &self.allowed_quote_mints,
            ) else {
                info!(
                    venue = %self.venue.slug(),
                    signature = %signature,
                    lb_pair = %lb_pair_event.lb_pair,
                    token_x = %lb_pair_event.token_x,
                    token_y = %lb_pair_event.token_y,
                    bin_step = lb_pair_event.bin_step,
                    "initialize_lb_pair2 detected but pair does not match quote filter"
                );
                continue;
            };

            if runtime
                .event_tx
                .send(VenueEvent {
                    venue: self.venue.clone(),
                    signature,
                    pool_id: lb_pair_event.lb_pair.clone(),
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
                pool_id = %lb_pair_event.lb_pair,
                token_x = %lb_pair_event.token_x,
                token_y = %lb_pair_event.token_y,
                bin_step = lb_pair_event.bin_step,
                "decoded lbPairCreate self-cpi event"
            );
        }

        warn!(venue = %self.venue.slug(), "meteora dlmm logs stream ended");
        Ok(())
    }
}

impl VenueWatcher for MeteoraDlmmWatcher {
    fn name(&self) -> &'static str {
        "meteora_dlmm"
    }

    fn spawn(self: Box<Self>, runtime: VenueRuntime) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(err) = self.run(runtime).await {
                error!(?err, "meteora dlmm logs listener failed");
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

fn log_has_initialize_lb_pair2(logs: &[String]) -> bool {
    logs.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("instruction: initialize_lb_pair2")
            || lower.contains("instruction: initializelbpair2")
    })
}

async fn fetch_lb_pair_create_from_signature(
    http_client: &HttpClient,
    rpc_url: &str,
    signature: &str,
) -> Result<Option<LbPairCreateEvent>> {
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
    if !has_initialize_lb_pair2_instruction(&tx_json) {
        return Ok(None);
    }

    Ok(extract_lb_pair_create_from_get_transaction_json(&tx_json))
}

async fn fetch_lb_pair_create_from_signature_with_retry(
    http_client: &HttpClient,
    rpc_url: &str,
    signature: &str,
) -> Result<Option<LbPairCreateEvent>> {
    let attempts = 6usize;
    let mut last_err: Option<anyhow::Error> = None;

    for i in 0..attempts {
        match fetch_lb_pair_create_from_signature(http_client, rpc_url, signature).await {
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

fn has_initialize_lb_pair2_instruction(tx_json: &Value) -> bool {
    let Some(instructions) = message_instructions(tx_json) else {
        return false;
    };

    instructions.iter().any(|ix| {
        decode_instruction_data(ix)
            .map(|raw| raw.starts_with(&INITIALIZE_LB_PAIR2_IX_DISCRIMINATOR))
            .unwrap_or(false)
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

fn extract_lb_pair_create_from_get_transaction_json(tx_json: &Value) -> Option<LbPairCreateEvent> {
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

            if let Some(ev) = parse_lb_pair_create_event_from_ix_data(&raw) {
                return Some(ev);
            }
        }
    }

    None
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

fn parse_lb_pair_create_event_from_ix_data(raw: &[u8]) -> Option<LbPairCreateEvent> {
    if raw.len() < EVENT_IX_TAG_LE.len() + 8 + 32 + 2 + 32 + 32 {
        return None;
    }

    if !raw.starts_with(EVENT_IX_TAG_LE) {
        return None;
    }

    let payload = &raw[EVENT_IX_TAG_LE.len()..];
    if !payload.starts_with(&LB_PAIR_CREATE_EVENT_DISCRIMINATOR) {
        return None;
    }

    let mut offset = LB_PAIR_CREATE_EVENT_DISCRIMINATOR.len();

    let lb_pair = parse_pubkey(payload, &mut offset)?;
    let bin_step = parse_u16(payload, &mut offset)?;
    let token_x = parse_pubkey(payload, &mut offset)?;
    let token_y = parse_pubkey(payload, &mut offset)?;

    Some(LbPairCreateEvent {
        lb_pair,
        bin_step,
        token_x,
        token_y,
    })
}

fn parse_u16(data: &[u8], offset: &mut usize) -> Option<u16> {
    if *offset + 2 > data.len() {
        return None;
    }
    let bytes: [u8; 2] = data[*offset..*offset + 2].try_into().ok()?;
    *offset += 2;
    Some(u16::from_le_bytes(bytes))
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
    fn decodes_lb_pair_create_from_event_cpi_instruction_data() {
        let lb_pair = Pubkey::from_str("HeMxQd26vTNGk4xriSJM8rQHSy4wTFpvE3vtajGiDFbV").unwrap();
        let token_x = Pubkey::from_str("Chb8N7pDdM72M6dpUKQHTM2YZB4TLEoiFZeyEb3Spump").unwrap();
        let token_y = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();

        let mut raw = Vec::new();
        raw.extend_from_slice(EVENT_IX_TAG_LE);
        raw.extend_from_slice(&LB_PAIR_CREATE_EVENT_DISCRIMINATOR);
        raw.extend_from_slice(lb_pair.as_ref());
        raw.extend_from_slice(&250u16.to_le_bytes());
        raw.extend_from_slice(token_x.as_ref());
        raw.extend_from_slice(token_y.as_ref());

        let parsed = parse_lb_pair_create_event_from_ix_data(&raw).unwrap();

        assert_eq!(parsed.lb_pair, lb_pair.to_string());
        assert_eq!(parsed.bin_step, 250);
        assert_eq!(parsed.token_x, token_x.to_string());
        assert_eq!(parsed.token_y, token_y.to_string());
    }

    #[test]
    fn rejects_non_event_cpi_payload() {
        let raw = vec![0_u8; 64];
        assert!(parse_lb_pair_create_event_from_ix_data(&raw).is_none());
    }

    #[test]
    fn detects_initialize_ix_in_helius_transaction_shape() {
        let init_ix_data = bs58::encode(INITIALIZE_LB_PAIR2_IX_DISCRIMINATOR).into_string();
        let tx_json = json!({
            "result": {
                "transaction": {
                    "message": {
                        "instructions": [
                            {
                                "programId": "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
                                "data": init_ix_data
                            }
                        ]
                    }
                }
            }
        });

        assert!(has_initialize_lb_pair2_instruction(&tx_json));
    }

    #[test]
    fn extracts_event_from_helius_inner_instruction_shape() {
        let lb_pair = Pubkey::from_str("HeMxQd26vTNGk4xriSJM8rQHSy4wTFpvE3vtajGiDFbV").unwrap();
        let token_x = Pubkey::from_str("Chb8N7pDdM72M6dpUKQHTM2YZB4TLEoiFZeyEb3Spump").unwrap();
        let token_y = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();

        let mut event_ix_data = Vec::new();
        event_ix_data.extend_from_slice(EVENT_IX_TAG_LE);
        event_ix_data.extend_from_slice(&LB_PAIR_CREATE_EVENT_DISCRIMINATOR);
        event_ix_data.extend_from_slice(lb_pair.as_ref());
        event_ix_data.extend_from_slice(&250u16.to_le_bytes());
        event_ix_data.extend_from_slice(token_x.as_ref());
        event_ix_data.extend_from_slice(token_y.as_ref());

        let tx_json = json!({
            "result": {
                "meta": {
                    "innerInstructions": [
                        {
                            "instructions": [
                                {
                                    "programId": "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
                                    "data": bs58::encode(event_ix_data).into_string()
                                }
                            ]
                        }
                    ]
                }
            }
        });

        let parsed = extract_lb_pair_create_from_get_transaction_json(&tx_json).unwrap();
        assert_eq!(parsed.lb_pair, lb_pair.to_string());
        assert_eq!(parsed.bin_step, 250);
        assert_eq!(parsed.token_x, token_x.to_string());
        assert_eq!(parsed.token_y, token_y.to_string());
    }
}
