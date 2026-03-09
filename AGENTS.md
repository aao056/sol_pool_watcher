# AGENTS.md

Operational guide for coding agents working in this repository.

## Project purpose

`sol_pool_listener` is a multi-venue Solana pool discovery and tracking service.

It does three core jobs:
- Detect newly created pools from on-chain venue logs.
- Enrich pools (metadata, risk, holder/tradeability context).
- Track pools and emit Telegram notifications (discovery + simulation signals).

It is not the execution bot. Trading execution lives in `liquidity_farmer`.

## Entry points

- Binary entry: `/Users/axog/Desktop/sol_pool_listener/src/main.rs`
- Pipeline: `/Users/axog/Desktop/sol_pool_listener/src/pipeline.rs`
- Persistent pool DB: `/Users/axog/Desktop/sol_pool_listener/src/pool_store.rs`
- Tracking engine: `/Users/axog/Desktop/sol_pool_listener/src/tracking/manager.rs`

## Runtime modes

CLI supports:
- `--mode normal` (default)
- `--mode sim`

Behavior:
- `normal`: discovery/update notifications active; tracking optional from config.
- `sim`: forces tracking enabled and routes GO/shadow messages to sim thread.

## Core architecture

1. Venue watchers (`/Users/axog/Desktop/sol_pool_listener/src/venues/*`) emit `VenueEvent`.
2. Pipeline stores pools in SQLite and computes multi-dex eligibility (2+ dexes per token/quote).
3. Services enrich events (`metadata`, `rugcheck`, analytics).
4. Tracking manager scores pools, manages state transitions, and emits NEW/UPDATE/GO/shadow messages.
5. Notifier sends messages to Telegram with optional topic/thread routing.

## Storage

SQLite schema lives in `pool_store`:
- `pools`: per-pool record (`first_seen_unix`, `last_seen_unix`, signatures, venue).
- `pair_candidates`: token/quote aggregate (`dex_count`, `pool_count`, `eligible`).

DB path:
- `POOL_DB_PATH` env var (default: `data/pools.db`).

## Config and env

Config file:
- `/Users/axog/Desktop/sol_pool_listener/cfg.example.toml`

Main env vars:
- `HELIUS_API_KEY`
- `POOL_DB_PATH` (optional)
- `TG_BOT_API_TOKEN` (optional)
- `TG_CHAT_ID` (optional)
- `TG_MESSAGE_THREAD_ID` (optional, default event thread)
- `TG_ERROR_MESSAGE_THREAD_ID` (optional, error thread)
- `TG_SIM_THREAD_ID` (optional, sim/GO/shadow thread)

## Notification routing

Notifier methods:
- `send_event` -> default thread
- `send_error` -> error thread (fallback: default)
- `send_sim` -> sim thread (fallback: default)

Current intent:
- Pool discovery / update: default thread
- GO + shadow entry/exit: sim thread

## Development commands

From `/Users/axog/Desktop/sol_pool_listener`:

- `cargo check`
- `cargo fmt`
- `cargo clippy -- -D warnings`
- `cargo run -- --cfg cfg.example.toml`
- `cargo run -- --cfg cfg.example.toml --mode sim`

## Safety rules

- Do not hardcode secrets or API keys in code/config.
- Do not remove risk/tradeability checks silently.
- Keep Telegram routing stable unless explicitly requested.
- Avoid destructive DB operations unless requested.
- If changing schema, include migration-safe handling for existing DB files.

## Agent editing guidelines

- Keep changes small and localized.
- Preserve existing event shapes (`VenueEvent`, tracking view structs) unless needed.
- When adding a new venue:
  1. add watcher in `src/venues`
  2. register in `src/venues/mod.rs`
  3. wire config toggles in `src/config.rs`
  4. ensure quote/token selection behavior is deterministic
- Prefer explicit logging around retry/backoff and decode failures.

## Integration boundary with `liquidity_farmer`

This service discovers and scores pools.
Execution should consume curated output/state (DB or generated config), not mix execution logic into listener watchers.

