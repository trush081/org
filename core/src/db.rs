//! Connection layer. The ONE place that knows about libSQL.
//!
//! Everything else in `core` takes a `&Db` and runs SQL through it, so the day
//! we point at a remote/synced Turso URL instead of a local file, only
//! `Config`/`Db::open` change — no call site does.

use crate::model::Result;
use std::path::PathBuf;

/// Where the database lives and how to reach it.
///
/// Today this only resolves a local file path. The libSQL crate also speaks to
/// remote/embedded-replica databases; when we want cloud sync, this enum grows
/// a `Remote { url, auth_token }` variant and `Db::open` matches on it. That's
/// the "config change, not a rewrite" the project brief asked for.
#[derive(Debug, Clone)]
pub enum Config {
    /// A local SQLite-compatible file on disk.
    Local(PathBuf),
    /// An in-memory database — used by tests so each test is isolated.
    Memory,
}

impl Config {
    /// Resolve the DB location with the precedence the brief specified:
    /// explicit `--file` flag  >  `$ORG_DB` env var  >  default `~/.org/org.db`.
    pub fn resolve(flag: Option<PathBuf>) -> Self {
        if let Some(path) = flag {
            return Config::Local(path);
        }
        if let Ok(env_path) = std::env::var("ORG_DB") {
            return Config::Local(PathBuf::from(env_path));
        }
        Config::Local(Self::default_path())
    }

    /// `~/.org/org.db`, falling back to a relative path if HOME is unset.
    fn default_path() -> PathBuf {
        // std::env::home_dir was undeprecated in 1.86; fine to use again.
        let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join(".org").join("org.db")
    }
}

/// Owns the libSQL connection. `core` functions borrow `&Db`.
pub struct Db {
    conn: libsql::Connection,
}

impl Db {
    /// Open (creating if needed), enable foreign keys, and run the schema.
    pub async fn open(config: Config) -> Result<Self> {
        // libsql::Builder builds a Database; we then take one Connection from it.
        // For a local file we ensure the parent dir exists first.
        let db = match &config {
            Config::Local(path) => {
                if let Some(parent) = path.parent() {
                    // Ignore "already exists"; surface real IO errors as Invalid.
                    if !parent.as_os_str().is_empty() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                }
                libsql::Builder::new_local(path).build().await?
            }
            Config::Memory => libsql::Builder::new_local(":memory:").build().await?,
        };

        let conn = db.connect()?;
        let me = Db { conn };
        me.bootstrap().await?;
        Ok(me)
    }

    /// Convenience for tests: a fresh in-memory DB.
    pub async fn open_memory() -> Result<Self> {
        Self::open(Config::Memory).await
    }

    /// Expose the raw connection to sibling modules within `core`.
    pub(crate) fn conn(&self) -> &libsql::Connection {
        &self.conn
    }

    /// Per-connection setup + idempotent schema. Safe to call on every open.
    async fn bootstrap(&self) -> Result<()> {
        // foreign_keys is OFF by default in SQLite/libSQL — without this,
        // ON DELETE CASCADE silently does nothing. Must be set per connection.
        self.conn.execute("PRAGMA foreign_keys = ON;", ()).await?;

        // execute_batch runs several statements in one call. CREATE ... IF NOT
        // EXISTS makes the whole thing idempotent: fresh DB gets built, existing
        // DB is untouched. This is our "migration approach" for v1 — no crate.
        self.conn.execute_batch(SCHEMA).await?;
        Ok(())
    }

    /// Load the project's seed data. Used to verify the reference CTEs.
    /// Returns silently if `people` already has rows (so it's safe to re-run).
    pub async fn load_seed(&self) -> Result<()> {
        let mut rows = self.conn.query("SELECT COUNT(*) FROM people", ()).await?;
        if let Some(row) = rows.next().await? {
            let count: i64 = row.get(0)?;
            if count > 0 {
                return Ok(());
            }
        }
        self.conn.execute_batch(SEED).await?;
        Ok(())
    }
}

/// Embedded schema. Indexes are composite `(id, kind)` because every traversal
/// query pairs an id with a kind filter, so the planner can use the index for
/// both predicates instead of scan-then-filter.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS people (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL,
  team        TEXT,
  title       TEXT,
  notes       TEXT,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS relationships (
  id          INTEGER PRIMARY KEY,
  from_id     INTEGER NOT NULL REFERENCES people(id) ON DELETE CASCADE,
  to_id       INTEGER NOT NULL REFERENCES people(id) ON DELETE CASCADE,
  kind        TEXT NOT NULL,
  notes       TEXT,
  confidence  REAL NOT NULL DEFAULT 1.0,
  source      TEXT NOT NULL DEFAULT 'manual',
  created_at  TEXT NOT NULL,
  UNIQUE(from_id, to_id, kind)
);

CREATE INDEX IF NOT EXISTS idx_rel_from ON relationships(from_id, kind);
CREATE INDEX IF NOT EXISTS idx_rel_to   ON relationships(to_id, kind);
"#;

/// Project seed data from the brief. Mirrors the expected-CTE fixtures.
const SEED: &str = r#"
INSERT INTO people (id, name, team, title, created_at, updated_at) VALUES
  (1, 'Dana Cruz',  'Marketing Delivery Tracking', 'Director',            '2026-01-01', '2026-01-01'),
  (2, 'Tom West',   'Marketing Delivery Tracking', 'Analyst',             '2026-01-01', '2026-01-01'),
  (3, 'Ana Ruiz',   'Marketing Delivery Tracking', 'Analyst II',          '2026-01-01', '2026-01-01'),
  (4, 'Pat Smith',  'IDS Fulfillment',             'Engineering Manager', '2026-01-01', '2026-01-01'),
  (5, 'Trent Rush', 'IDS Fulfillment',             'Sr Engineer',         '2026-01-01', '2026-01-01'),
  (6, 'Jane Doe',   'IDS Fulfillment',             'SWE II',              '2026-01-01', '2026-01-01'),
  (7, 'Mike Chen',  'IDS Fulfillment',             'SWE I',               '2026-01-01', '2026-01-01');

INSERT INTO relationships (from_id, to_id, kind, confidence, source, created_at) VALUES
  (2, 1, 'reports_to', 1.0, 'manual', '2026-01-01'),
  (3, 1, 'reports_to', 1.0, 'manual', '2026-01-01'),
  (5, 4, 'reports_to', 1.0, 'manual', '2026-01-01'),
  (6, 4, 'reports_to', 1.0, 'manual', '2026-01-01'),
  (7, 4, 'reports_to', 1.0, 'manual', '2026-01-01');
"#;
