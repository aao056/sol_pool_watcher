# sol_pool_listener

Multi-venue Solana new-pool listener with Telegram notifications.

## Current status

Implemented watcher:
- Raydium CLMM `CreatePool`
- Raydium AMM `initialize2`
- Meteora DAMM `initialize_pool` (via self-CPI `EvtInitializePool` event decoding)
- Meteora DLMM `initialize_lb_pair2` (via self-CPI `LbPairCreate` event decoding)
- PumpSwap `create_pool` (with optional self-CPI `CreatePoolEvent` enrichment)
- Orca Whirlpool `initialize_pool_v2` (with optional self-CPI `PoolInitialized` event enrichment)

## What it does

- subscribes to venue logs (Raydium CLMM + Meteora DAMM + Meteora DLMM + PumpSwap + Orca Whirlpool)
- resolves `pool_id` + token mints
- filters to configured quote mints (default SOL/USDC)
- enriches with token metadata and RugCheck report
- tracks new pools in a short-lived state machine (`NEW -> TRACKING -> QUIET`)
- computes a compact `SniperScore` and emits update notifications on score/liquidity-risk changes
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
program_id = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8"
allowed_quote_mints = [
  "So11111111111111111111111111111111111111112", # SOL
  "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", # USDC
]

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

[tracking]
enabled = true
tick_interval_secs = 10
refresh_interval_secs = 20
light_tracking_secs = 60
tracking_secs = 600
notify_cooldown_secs = 60
update_score_delta_threshold = 10
liquidity_drop_alert_ratio = 0.15
promote_min_score = 55
promote_min_liquidity_usd = 20000.0
promote_require_low_risk = true
shadow_enabled = true
shadow_take_profit_bps = 2000
shadow_stop_loss_bps = 1200
shadow_max_hold_secs = 1200

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

## Telegram notifications (Phase 1 tracker)

- NEW message includes: `SniperScore`, pool age, liquidity snapshot + delta, tradeability checks, holder concentration, and compact `Flags`
- NEW/UPDATE messages now include `Flow 1m/5m` and `Impact` lines (Phase 2A)
- UPDATE message is sent only when:
  - score changes by `>= update_score_delta_threshold`
  - rapid liquidity drop trigger fires
  - mint/freeze/transfer-fee tradeability flags change

Phase 1 uses best-effort enrichment with fallbacks:
- tradeability checks are on-chain (`getAccountInfo` jsonParsed)
- holder concentration is on-chain (`getTokenLargestAccounts` + supply fallback)
- liquidity currently uses best-effort RugCheck snapshots for deltas until venue-specific reserve math is added
- swap-flow windows are implemented, but if a venue swap decoder is not wired yet you will see `no swaps seen`
- CPMM `Impact` is estimated for stable-quoted CPMM venues using a conservative liquidity-based approximation (`~`) until exact reserve snapshots are wired

## Extension architecture

Venue integration flow:
1. Add a watcher implementing `VenueWatcher` in `src/venues/...`
2. Emit `VenueEvent` into the shared pipeline
3. Register watcher in `build_watchers` (`src/venues/mod.rs`)

Shared enrichment + output pipeline stays unchanged.
