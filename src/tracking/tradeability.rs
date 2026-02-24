use crate::tracking::model::{TokenProgramKind, TradeabilityProfile};
use reqwest::Client as HttpClient;
use serde_json::{Value, json};

const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

pub async fn fetch_tradeability_profile(
    http_client: &HttpClient,
    rpc_url: &str,
    token_mint: &str,
) -> Option<TradeabilityProfile> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [token_mint, {"encoding": "jsonParsed", "commitment": "confirmed"}]
    });

    let response = http_client.post(rpc_url).json(&body).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value: Value = response.json().await.ok()?;
    if value.get("error").is_some() {
        return None;
    }
    parse_tradeability_profile(&value)
}

pub fn parse_tradeability_profile(root: &Value) -> Option<TradeabilityProfile> {
    let owner = root
        .pointer("/result/value/owner")
        .and_then(Value::as_str)?;
    let info = root.pointer("/result/value/data/parsed/info")?;

    let token_program = Some(match owner {
        TOKEN_PROGRAM_ID => TokenProgramKind::SplToken,
        TOKEN_2022_PROGRAM_ID => TokenProgramKind::Token2022,
        other => TokenProgramKind::Other(other.to_string()),
    });

    let mint_authority_active = match info.get("mintAuthority") {
        Some(Value::Null) => Some(false),
        Some(Value::String(s)) if !s.trim().is_empty() => Some(true),
        Some(_) => Some(false),
        None => None,
    };
    let freeze_authority_active = match info.get("freezeAuthority") {
        Some(Value::Null) => Some(false),
        Some(Value::String(s)) if !s.trim().is_empty() => Some(true),
        Some(_) => Some(false),
        None => None,
    };

    let transfer_fee = match &token_program {
        Some(TokenProgramKind::Token2022) => {
            let ext = info.get("extensions");
            Some(ext.is_some_and(value_contains_transfer_fee))
        }
        Some(TokenProgramKind::SplToken) => Some(false),
        _ => None,
    };

    Some(TradeabilityProfile {
        mint_authority_active,
        freeze_authority_active,
        token_program,
        token2022_transfer_fee_extension: transfer_fee,
        metadata_mutable: None,
    })
}

fn value_contains_transfer_fee(v: &Value) -> bool {
    match v {
        Value::String(s) => s.to_ascii_lowercase().contains("transferfee"),
        Value::Array(arr) => arr.iter().any(value_contains_transfer_fee),
        Value::Object(map) => map.iter().any(|(k, val)| {
            k.to_ascii_lowercase().contains("transferfee") || value_contains_transfer_fee(val)
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_tradeability_profile;
    use crate::tracking::model::TokenProgramKind;
    use serde_json::json;

    #[test]
    fn parses_spl_token_mint_authorities() {
        let v = json!({
            "result": {
                "value": {
                    "owner": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
                    "data": {
                        "parsed": {
                            "info": {
                                "mintAuthority": null,
                                "freezeAuthority": "SomeFreeze"
                            }
                        }
                    }
                }
            }
        });

        let p = parse_tradeability_profile(&v).unwrap();
        assert_eq!(p.mint_authority_active, Some(false));
        assert_eq!(p.freeze_authority_active, Some(true));
        assert!(matches!(p.token_program, Some(TokenProgramKind::SplToken)));
        assert_eq!(p.token2022_transfer_fee_extension, Some(false));
    }

    #[test]
    fn detects_token2022_transfer_fee_extension() {
        let v = json!({
            "result": {
                "value": {
                    "owner": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
                    "data": {
                        "parsed": {
                            "info": {
                                "mintAuthority": "Auth",
                                "freezeAuthority": null,
                                "extensions": [
                                    {"extension": "transferFeeConfig"}
                                ]
                            }
                        }
                    }
                }
            }
        });

        let p = parse_tradeability_profile(&v).unwrap();
        assert_eq!(p.mint_authority_active, Some(true));
        assert_eq!(p.freeze_authority_active, Some(false));
        assert!(matches!(p.token_program, Some(TokenProgramKind::Token2022)));
        assert_eq!(p.token2022_transfer_fee_extension, Some(true));
    }
}
