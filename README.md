# sol_pool_listener

Multi-venue Solana new-pool listener with Telegram notifications.

## Current status

Implemented watcher:
- Raydium CLMM `CreatePool`
- Meteora DAMM `initialize_pool` (via self-CPI `EvtInitializePool` event decoding)
- Meteora DLMM `initialize_lb_pair2` (via self-CPI `LbPairCreate` event decoding)
- PumpSwap `create_pool` (with optional self-CPI `CreatePoolEvent` enrichment)
- Orca Whirlpool `initialize_pool_v2` (with optional self-CPI `PoolInitialized` event enrichment)

Architecture ready (scaffolded, not implemented yet):
- Raydium AMM

## What it does

- subscribes to venue logs (Raydium CLMM + Meteora DAMM + Meteora DLMM + PumpSwap + Orca Whirlpool)
- resolves `pool_id` + token mints
- filters to configured quote mints (default SOL/USDC)
- enriches with token metadata and RugCheck report
- prints ready `[[pools]]` snippet
- optionally sends Telegram notification
- runs periodic health checks (RPC ping + task liveness snapshot)

## Quick start

1. Copy config:

```bash
cp cfg.example.toml cfg.toml
```

2. Configure venues in `cfg.toml`.

3. Set env vars:

```bash
cp .env.example .env
```

4. Run:

```bash
cargo run -- --cfg cfg.toml
```

## Config

```toml
[venues.raydium_clmm]
enabled = true
program_id = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK"
allowed_quote_mints = [
  "So11111111111111111111111111111111111111112", # SOL
  "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", # USDC
]

[venues.raydium_amm]
enabled = false

[venues.meteora_damm]
enabled = false
program_id = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG"
allowed_quote_mints = [
  "So11111111111111111111111111111111111111112", # SOL
  "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", # USDC
]

[venues.meteora_dlmm]
enabled = false
program_id = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo"
allowed_quote_mints = [
  "So11111111111111111111111111111111111111112", # SOL
  "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", # USDC
]

[venues.pumpswap]
enabled = false
program_id = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
allowed_quote_mints = [
  "So11111111111111111111111111111111111111112", # SOL
  "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", # USDC
]

[venues.orca_whirlpool]
enabled = false
program_id = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc"
allowed_quote_mints = [
  "So11111111111111111111111111111111111111112", # SOL
  "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", # USDC
]

```

Backward compatibility is kept for legacy `[raydium]` CLMM keys (`clmm_program_id`, `allowed_quote_mints`).

## Environment

Required:

```env
HELIUS_API_KEY=YOUR_KEY
```

Optional Telegram:

```env
TG_BOT_API_TOKEN=123456:YOUR_BOT_TOKEN
TG_CHAT_ID=123456789
```

## Extension architecture

Venue integration flow:
1. Add a watcher implementing `VenueWatcher` in `src/venues/...`
2. Emit `VenueEvent` into the shared pipeline
3. Register watcher in `build_watchers` (`src/venues/mod.rs`)

Shared enrichment + output pipeline stays unchanged.