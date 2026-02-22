use crate::constants::TOKEN_METADATA_PROGRAM_ID;
use crate::domain::TokenMetadataLite;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use std::str::FromStr;

fn sanitize_metadata_string(raw: String) -> Option<String> {
    let cleaned = raw.trim_end_matches('\0').trim().to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn parse_borsh_string(data: &[u8], offset: &mut usize) -> Option<String> {
    if *offset + 4 > data.len() {
        return None;
    }
    let len = u32::from_le_bytes([
        data[*offset],
        data[*offset + 1],
        data[*offset + 2],
        data[*offset + 3],
    ]) as usize;
    *offset += 4;
    if *offset + len > data.len() {
        return None;
    }
    let s = String::from_utf8_lossy(&data[*offset..*offset + len]).to_string();
    *offset += len;
    sanitize_metadata_string(s)
}

pub async fn fetch_token_metadata(rpc: &RpcClient, token_mint: &str) -> Option<TokenMetadataLite> {
    let metadata_program = Pubkey::from_str(TOKEN_METADATA_PROGRAM_ID).ok()?;
    let mint = Pubkey::from_str(token_mint).ok()?;
    let (metadata_pda, _) = Pubkey::find_program_address(
        &[b"metadata", metadata_program.as_ref(), mint.as_ref()],
        &metadata_program,
    );

    let account = rpc.get_account(&metadata_pda).await.ok()?;
    let data = account.data;
    if data.len() < 1 + 32 + 32 + 4 {
        return None;
    }

    let mut offset = 1 + 32 + 32;
    let name = parse_borsh_string(&data, &mut offset);
    let symbol = parse_borsh_string(&data, &mut offset);

    Some(TokenMetadataLite { name, symbol })
}
