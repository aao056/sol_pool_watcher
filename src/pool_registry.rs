use crate::domain::{VenueEvent, symbol_for_mint};
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{info, warn};

#[derive(Clone, Debug)]
pub struct CrossVenuePairMatch {
    pub token_mint: String,
    pub quote_mint: String,
    pub rows: Vec<PoolRegistryRow>,
    pub unique_venues: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PoolRegistryRow {
    pub venue_slug: String,
    pub venue_display: String,
    pub pool_id: String,
    pub _first_seen_unix: u64,
}

#[derive(Clone, Debug)]
pub struct PoolRegistry {
    db_path: PathBuf,
}

impl PoolRegistry {
    pub fn new<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create db dir {}", parent.display()))?;
        }

        let registry = Self { db_path };
        registry.init_schema()?;
        info!(db_path = %registry.db_path.display(), "sqlite pool registry initialized");
        Ok(registry)
    }

    pub fn try_new_default() -> Option<Self> {
        let db_path = std::env::var("POOLS_DB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/pools.db"));

        match Self::new(db_path) {
            Ok(db) => Some(db),
            Err(err) => {
                warn!(?err, "sqlite pool registry disabled");
                None
            }
        }
    }

    pub fn insert_pool_and_match(
        &self,
        event: &VenueEvent,
        first_seen_unix: u64,
    ) -> Result<Option<CrossVenuePairMatch>> {
        let venue_slug = event.venue.slug();

        let insert_sql = format!(
            "INSERT OR IGNORE INTO pools (venue_slug, venue_display, pool_id, token_mint, quote_mint, first_seen_unix, signature) \
             VALUES ({}, {}, {}, {}, {}, {}, {});",
            sql_quote(&venue_slug),
            sql_quote(&event.venue.display_name),
            sql_quote(&event.pool_id),
            sql_quote(&event.token_mint),
            sql_quote(&event.quote_mint),
            first_seen_unix,
            sql_quote(&event.signature),
        );
        self.exec_sql(&insert_sql)?;

        let query_sql = format!(
            "SELECT venue_slug, venue_display, pool_id, first_seen_unix \
             FROM pools \
             WHERE token_mint = {} AND quote_mint = {} \
             ORDER BY first_seen_unix ASC;",
            sql_quote(&event.token_mint),
            sql_quote(&event.quote_mint),
        );
        let output = self.query_tsv(&query_sql)?;
        let mut rows = parse_registry_rows(&output)?;
        if rows.is_empty() {
            return Ok(None);
        }

        let mut venues_seen = HashSet::new();
        let mut unique_venues = Vec::new();
        for row in &rows {
            if venues_seen.insert(row.venue_slug.clone()) {
                unique_venues.push(row.venue_slug.clone());
            }
        }
        if unique_venues.len() < 2 {
            return Ok(None);
        }

        // Ensure this alert is caused by a new venue presence, not same-venue duplicate pools only.
        let current_venue_has_other = rows
            .iter()
            .any(|row| row.venue_slug == venue_slug && row.pool_id != event.pool_id);
        let other_venue_exists = rows.iter().any(|row| row.venue_slug != venue_slug);
        if !other_venue_exists && !current_venue_has_other {
            return Ok(None);
        }

        rows.retain(|row| !row.pool_id.is_empty());
        Ok(Some(CrossVenuePairMatch {
            token_mint: event.token_mint.clone(),
            quote_mint: event.quote_mint.clone(),
            rows,
            unique_venues,
        }))
    }

    pub fn enqueue_cross_venue_candidate(
        &self,
        pair_match: &CrossVenuePairMatch,
        detected_at_unix: u64,
    ) -> Result<i64> {
        let venues_count = i64::try_from(pair_match.unique_venues.len())
            .context("venues_count overflow when enqueuing arb candidate")?;

        let upsert_sql = format!(
            "INSERT INTO arb_candidates \
             (token_mint, quote_mint, status, priority, first_detected_unix, last_detected_unix, venues_count) \
             VALUES ({}, {}, 'new', {}, {}, {}, {}) \
             ON CONFLICT(token_mint, quote_mint) DO UPDATE SET \
               last_detected_unix = excluded.last_detected_unix, \
               venues_count = CASE \
                 WHEN excluded.venues_count > arb_candidates.venues_count THEN excluded.venues_count \
                 ELSE arb_candidates.venues_count END, \
               priority = CASE \
                 WHEN excluded.priority > arb_candidates.priority THEN excluded.priority \
                 ELSE arb_candidates.priority END, \
               updated_at = CURRENT_TIMESTAMP;",
            sql_quote(&pair_match.token_mint),
            sql_quote(&pair_match.quote_mint),
            venues_count,
            detected_at_unix,
            detected_at_unix,
            venues_count,
        );
        self.exec_sql(&upsert_sql)?;

        let candidate_id_sql = format!(
            "SELECT id FROM arb_candidates WHERE token_mint = {} AND quote_mint = {} LIMIT 1;",
            sql_quote(&pair_match.token_mint),
            sql_quote(&pair_match.quote_mint),
        );
        let candidate_id = self
            .query_scalar_i64(&candidate_id_sql)?
            .context("arb candidate id not found after upsert")?;

        if !pair_match.rows.is_empty() {
            let values = pair_match
                .rows
                .iter()
                .map(|row| {
                    format!(
                        "({}, {}, {}, {})",
                        candidate_id,
                        sql_quote(&row.venue_slug),
                        sql_quote(&row.pool_id),
                        row._first_seen_unix
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");

            let link_sql = format!(
                "INSERT OR IGNORE INTO arb_candidate_pools \
                 (candidate_id, venue_slug, pool_id, first_seen_unix) VALUES {};",
                values
            );
            self.exec_sql(&link_sql)?;
        }

        Ok(candidate_id)
    }

    fn init_schema(&self) -> Result<()> {
        let schema = r#"
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA foreign_keys=ON;
PRAGMA busy_timeout=5000;

CREATE TABLE IF NOT EXISTS pools (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  venue_slug TEXT NOT NULL,
  venue_display TEXT NOT NULL,
  pool_id TEXT NOT NULL,
  token_mint TEXT NOT NULL,
  quote_mint TEXT NOT NULL,
  first_seen_unix INTEGER NOT NULL,
  signature TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (venue_slug, pool_id)
);

CREATE INDEX IF NOT EXISTS idx_pools_token_quote
  ON pools (token_mint, quote_mint);

CREATE INDEX IF NOT EXISTS idx_pools_token
  ON pools (token_mint);

CREATE INDEX IF NOT EXISTS idx_pools_first_seen
  ON pools (first_seen_unix DESC);

CREATE TABLE IF NOT EXISTS arb_candidates (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  token_mint TEXT NOT NULL,
  quote_mint TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'new',
  priority INTEGER NOT NULL DEFAULT 0,
  first_detected_unix INTEGER NOT NULL,
  last_detected_unix INTEGER NOT NULL,
  venues_count INTEGER NOT NULL DEFAULT 0,
  claimed_by TEXT,
  lease_until_unix INTEGER,
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (token_mint, quote_mint)
);

CREATE INDEX IF NOT EXISTS idx_arb_candidates_status
  ON arb_candidates (status, priority DESC, last_detected_unix DESC);

CREATE INDEX IF NOT EXISTS idx_arb_candidates_pair
  ON arb_candidates (token_mint, quote_mint);

CREATE TABLE IF NOT EXISTS arb_candidate_pools (
  candidate_id INTEGER NOT NULL,
  venue_slug TEXT NOT NULL,
  pool_id TEXT NOT NULL,
  first_seen_unix INTEGER NOT NULL,
  PRIMARY KEY (candidate_id, venue_slug, pool_id),
  FOREIGN KEY(candidate_id) REFERENCES arb_candidates(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_arb_candidate_pools_candidate
  ON arb_candidate_pools (candidate_id);
"#;
        self.exec_sql(schema)
    }

    fn exec_sql(&self, sql: &str) -> Result<()> {
        let status = Command::new("sqlite3")
            .arg(&self.db_path)
            .arg(sql)
            .status()
            .context("failed to run sqlite3")?;
        if !status.success() {
            bail!("sqlite3 exited with status {status}");
        }
        Ok(())
    }

    fn query_tsv(&self, sql: &str) -> Result<String> {
        let output = Command::new("sqlite3")
            .arg("-noheader")
            .arg("-separator")
            .arg("\t")
            .arg(&self.db_path)
            .arg(sql)
            .output()
            .context("failed to run sqlite3 query")?;
        if !output.status.success() {
            bail!("sqlite3 query exited with status {}", output.status);
        }
        let stdout =
            String::from_utf8(output.stdout).context("sqlite3 query output was not utf8")?;
        Ok(stdout)
    }

    fn query_scalar_i64(&self, sql: &str) -> Result<Option<i64>> {
        let output = self.query_tsv(sql)?;
        let first = output.lines().map(str::trim).find(|line| !line.is_empty());
        let Some(first) = first else {
            return Ok(None);
        };
        let v = first
            .parse::<i64>()
            .with_context(|| format!("sqlite scalar was not i64: {first}"))?;
        Ok(Some(v))
    }
}

pub fn build_cross_venue_alert(event: &VenueEvent, m: &CrossVenuePairMatch) -> String {
    let token_symbol = symbol_for_mint(&m.token_mint);
    let quote_symbol = symbol_for_mint(&m.quote_mint);
    let venues_list = m
        .rows
        .iter()
        .map(|r| {
            format!(
                "• {} — https://solscan.io/account/{}",
                r.venue_display, r.pool_id
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "⚡ Cross-venue pair detected\n\n\
👀 Token: {} (https://solscan.io/token/{})\n\
⚖️ Pair: {}/{}\n\
🆕 New venue: {} (https://solscan.io/account/{})\n\
🧭 Seen on {} venues: {}\n\n\
{}\n\n\
🧾 Tx: https://solscan.io/tx/{}",
        token_symbol,
        m.token_mint,
        token_symbol,
        quote_symbol,
        event.venue.display_name,
        event.pool_id,
        m.unique_venues.len(),
        m.unique_venues.join(", "),
        venues_list,
        event.signature,
    )
}

fn parse_registry_rows(tsv: &str) -> Result<Vec<PoolRegistryRow>> {
    let mut rows = Vec::new();
    for line in tsv.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts = line.split('\t').collect::<Vec<_>>();
        if parts.len() != 4 {
            bail!("unexpected sqlite row format: {line}");
        }
        let first_seen_unix = parts[3]
            .parse::<u64>()
            .with_context(|| format!("invalid first_seen_unix in row: {line}"))?;
        rows.push(PoolRegistryRow {
            venue_slug: parts[0].to_string(),
            venue_display: parts[1].to_string(),
            pool_id: parts[2].to_string(),
            _first_seen_unix: first_seen_unix,
        });
    }
    Ok(rows)
}

fn sql_quote(s: &str) -> String {
    let escaped = s.replace('\'', "''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tsv_rows() {
        let tsv = "meteora_damm\tMeteora DAMM\tPoolA\t123\npumpswap_amm\tPumpSwap\tPoolB\t124\n";
        let rows = parse_registry_rows(tsv).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].venue_slug, "meteora_damm");
        assert_eq!(rows[1].pool_id, "PoolB");
    }
}
