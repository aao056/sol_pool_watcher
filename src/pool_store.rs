use crate::domain::VenueEvent;
use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy)]
pub struct CandidateStatus {
    pub dex_count: u64,
    pub pool_count: u64,
    pub eligible: bool,
    pub became_eligible: bool,
}

pub struct PoolStore {
    conn: Mutex<Connection>,
}

impl PoolStore {
    pub fn open(path: &str) -> Result<Self> {
        if let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create pool DB parent dir: {}", parent.display())
            })?;
        }

        let conn =
            Connection::open(path).with_context(|| format!("failed to open pool DB at {path}"))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("failed to set pool DB journal mode")?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .context("failed to set pool DB synchronous mode")?;

        init_schema(&conn)?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn upsert_event(&self, event: &VenueEvent) -> Result<CandidateStatus> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("pool DB mutex poisoned"))?;

        let tx = conn.transaction().context("failed to start pool DB tx")?;
        let now = now_unix();

        let previous_eligible: Option<i64> = tx
            .query_row(
                "SELECT eligible
                 FROM pair_candidates
                 WHERE token_mint = ?1 AND quote_mint = ?2",
                params![event.token_mint, event.quote_mint],
                |row| row.get(0),
            )
            .optional()
            .context("failed to query previous pair eligibility")?;

        tx.execute(
            "INSERT INTO pools (
                 pool_id,
                 dex,
                 kind,
                 token_mint,
                 quote_mint,
                 first_signature,
                 last_signature,
                 first_seen_unix,
                 last_seen_unix
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?7)
             ON CONFLICT(pool_id) DO UPDATE SET
                 dex = excluded.dex,
                 kind = excluded.kind,
                 token_mint = excluded.token_mint,
                 quote_mint = excluded.quote_mint,
                 last_signature = excluded.last_signature,
                 last_seen_unix = excluded.last_seen_unix",
            params![
                event.pool_id,
                event.venue.dex,
                event.venue.kind,
                event.token_mint,
                event.quote_mint,
                event.signature,
                now,
            ],
        )
        .context("failed to upsert pool row")?;

        let (dex_count_i64, pool_count_i64): (i64, i64) = tx
            .query_row(
                "SELECT COUNT(DISTINCT dex), COUNT(*)
                 FROM pools
                 WHERE token_mint = ?1 AND quote_mint = ?2",
                params![event.token_mint, event.quote_mint],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .context("failed to aggregate pair stats")?;

        let eligible_i64 = if dex_count_i64 >= 2 { 1 } else { 0 };

        tx.execute(
            "INSERT INTO pair_candidates (
                 token_mint,
                 quote_mint,
                 dex_count,
                 pool_count,
                 eligible,
                 updated_unix
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(token_mint, quote_mint) DO UPDATE SET
                 dex_count = excluded.dex_count,
                 pool_count = excluded.pool_count,
                 eligible = excluded.eligible,
                 updated_unix = excluded.updated_unix",
            params![
                event.token_mint,
                event.quote_mint,
                dex_count_i64,
                pool_count_i64,
                eligible_i64,
                now,
            ],
        )
        .context("failed to upsert pair candidate row")?;

        tx.commit().context("failed to commit pool DB tx")?;

        let previous_eligible = previous_eligible.unwrap_or(0) != 0;
        let eligible = eligible_i64 != 0;

        Ok(CandidateStatus {
            dex_count: dex_count_i64.max(0) as u64,
            pool_count: pool_count_i64.max(0) as u64,
            eligible,
            became_eligible: !previous_eligible && eligible,
        })
    }
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pools (
             pool_id TEXT PRIMARY KEY,
             dex TEXT NOT NULL,
             kind TEXT NOT NULL,
             token_mint TEXT NOT NULL,
             quote_mint TEXT NOT NULL,
             first_signature TEXT NOT NULL,
             last_signature TEXT NOT NULL,
             first_seen_unix INTEGER NOT NULL,
             last_seen_unix INTEGER NOT NULL
         );

         CREATE INDEX IF NOT EXISTS idx_pools_token_quote
           ON pools(token_mint, quote_mint);

         CREATE INDEX IF NOT EXISTS idx_pools_last_seen
           ON pools(last_seen_unix DESC);

         CREATE TABLE IF NOT EXISTS pair_candidates (
             token_mint TEXT NOT NULL,
             quote_mint TEXT NOT NULL,
             dex_count INTEGER NOT NULL,
             pool_count INTEGER NOT NULL,
             eligible INTEGER NOT NULL,
             updated_unix INTEGER NOT NULL,
             PRIMARY KEY(token_mint, quote_mint)
         );

         CREATE INDEX IF NOT EXISTS idx_pair_candidates_eligible_updated
           ON pair_candidates(eligible, updated_unix DESC);",
    )
    .context("failed to initialize pool DB schema")?;

    Ok(())
}

fn now_unix() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs() as i64,
        Err(_) => 0,
    }
}
