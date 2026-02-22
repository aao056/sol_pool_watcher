use crate::domain::HolderShare;
use reqwest::Client as HttpClient;
use serde_json::{Value, json};

pub async fn fetch_top_holders(
    http_client: &HttpClient,
    rpc_url: &str,
    token_mint: &str,
    top_n: usize,
) -> Option<Vec<HolderShare>> {
    let largest_body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTokenLargestAccounts",
        "params": [token_mint, {"commitment": "confirmed"}]
    });
    let supply_body = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "getTokenSupply",
        "params": [token_mint, {"commitment": "confirmed"}]
    });

    let largest_resp = http_client
        .post(rpc_url)
        .json(&largest_body)
        .send()
        .await
        .ok()?;
    if !largest_resp.status().is_success() {
        return None;
    }
    let largest_json: Value = largest_resp.json().await.ok()?;
    if largest_json.get("error").is_some() {
        return None;
    }
    let accounts = largest_json.pointer("/result/value")?.as_array()?;

    let supply_resp = http_client
        .post(rpc_url)
        .json(&supply_body)
        .send()
        .await
        .ok()?;
    if !supply_resp.status().is_success() {
        return None;
    }
    let supply_json: Value = supply_resp.json().await.ok()?;
    if supply_json.get("error").is_some() {
        return None;
    }
    let supply_raw = supply_json
        .pointer("/result/value/amount")
        .and_then(Value::as_str)?;
    let supply = supply_raw.parse::<u128>().ok()?;
    if supply == 0 {
        return None;
    }

    let mut out = Vec::new();
    for row in accounts.iter().take(top_n) {
        let address = row.get("address").and_then(Value::as_str)?.to_string();
        let amount_raw = row.get("amount").and_then(Value::as_str)?;
        let amount = amount_raw.parse::<u128>().ok()?;
        let pct = (amount as f64) * 100.0 / (supply as f64);
        out.push(HolderShare { address, pct });
    }
    Some(out)
}
