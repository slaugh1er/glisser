//! Storage: SQLite in WAL mode.
//!
//! WARNING: a curated index of exactly the content that is risky. It belongs
//! on an encrypted volume that does not travel, and is crypto-erased after.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};

use crate::model::{Dialog, Hit, Message};

/// Bump on any change to SCHEMA.
const SCHEMA_VERSION: i64 = 5;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS dialogs (
  id            INTEGER PRIMARY KEY,
  kind          TEXT    NOT NULL,
  access_hash   INTEGER,
  title         TEXT,
  username      TEXT,
  archived      INTEGER NOT NULL DEFAULT 0,
  msg_count     INTEGER NOT NULL DEFAULT 0,
  synced_to_id  INTEGER
);

CREATE TABLE IF NOT EXISTS messages (
  dialog_id   INTEGER NOT NULL,
  msg_id      INTEGER NOT NULL,
  date        INTEGER NOT NULL,
  from_id     INTEGER,
  outgoing    INTEGER NOT NULL,
  reply_to    INTEGER,
  grouped_id  INTEGER,
  fwd_from    TEXT,
  media_type  TEXT,
  file_path   TEXT,
  text        TEXT NOT NULL DEFAULT '',
  -- The whitelist marks a message, not a hit: otherwise a clean holiday
  -- conversation (no hits at all) would stay unprotected and could travel
  -- into the plan inside someone else's range.
  protected   INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (dialog_id, msg_id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_msg_date  ON messages(dialog_id, date);
CREATE INDEX IF NOT EXISTS idx_msg_reply ON messages(dialog_id, reply_to);
CREATE INDEX IF NOT EXISTS idx_msg_group ON messages(dialog_id, grouped_id);

CREATE TABLE IF NOT EXISTS hits (
  dialog_id  INTEGER NOT NULL,
  msg_id     INTEGER NOT NULL,
  axis       TEXT    NOT NULL,
  rule_id    TEXT    NOT NULL,
  term       TEXT    NOT NULL,
  surface    TEXT,
  layer      TEXT    NOT NULL,
  tier       TEXT    NOT NULL,
  priority   REAL    NOT NULL,
  protected  INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (dialog_id, msg_id, rule_id, term)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_hits_tier ON hits(tier, axis);
CREATE INDEX IF NOT EXISTS idx_hits_msg  ON hits(dialog_id, msg_id);

CREATE TABLE IF NOT EXISTS transcripts (
  dialog_id INTEGER NOT NULL,
  msg_id    INTEGER NOT NULL,
  text      TEXT    NOT NULL,
  model     TEXT    NOT NULL,
  PRIMARY KEY (dialog_id, msg_id)
) WITHOUT ROWID;

-- The model's raw answer to a window, stored whole so that metrics can be
-- recomputed without new calls and the parsing can change afterwards.
-- prompt_id is part of the key, or a second version of the policy would
-- overwrite the first and there would be nothing to compare.
CREATE TABLE IF NOT EXISTS triage_runs (
  dialog_id         INTEGER NOT NULL,
  window_from       INTEGER NOT NULL,
  window_to         INTEGER NOT NULL,
  model             TEXT    NOT NULL,
  prompt_id         TEXT    NOT NULL,
  raw               TEXT    NOT NULL,
  prompt_tokens     INTEGER NOT NULL DEFAULT 0,
  completion_tokens INTEGER NOT NULL DEFAULT 0,
  cost              REAL    NOT NULL DEFAULT 0,
  -- how many times the window went to the model: 2 means it asked to widen
  passes            INTEGER NOT NULL DEFAULT 1,
  created_at        INTEGER NOT NULL,
  PRIMARY KEY (dialog_id, window_from, window_to, model, prompt_id)
) WITHOUT ROWID;

-- The only input purge has: one row is one message. Derived — assembled by
-- `plan` from the verdicts of one run and rebuilt as often as needed. Triage
-- never touches it, or a model bake-off would blend the verdicts.
CREATE TABLE IF NOT EXISTS plan (
  dialog_id  INTEGER NOT NULL,
  msg_id     INTEGER NOT NULL,
  reason     TEXT,
  axis       TEXT,
  confidence REAL,
  approved   INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (dialog_id, msg_id)
) WITHOUT ROWID;

-- Intent log. The row is written BEFORE the API call, so that a crash never
-- loses the trace of a deletion that was attempted.
CREATE TABLE IF NOT EXISTS deletions (
  dialog_id    INTEGER NOT NULL,
  msg_id       INTEGER NOT NULL,
  batch_id     TEXT    NOT NULL,
  state        TEXT    NOT NULL,
  attempted_at INTEGER NOT NULL,
  error        TEXT,
  PRIMARY KEY (dialog_id, msg_id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_del_state ON deletions(state);
"#;

pub struct Db {
    pub conn: Connection,
    /// Kept so that parallel triage can open one connection per thread:
    /// `Connection` is not `Sync`, SQLite in WAL is.
    pub path: std::path::PathBuf,
}

impl Db {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        // First run: state/ does not exist yet.
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("could not open the database {}", path.display()))?;

        // WAL: an interrupted batch must not leave the base half-written.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // WAL takes one writer anyway; verdicts land every few seconds per
        // thread. Without a timeout the wait would be an instant SQLITE_BUSY.
        conn.busy_timeout(std::time::Duration::from_secs(30))?;

        // `CREATE TABLE IF NOT EXISTS` leaves an existing table alone, so a
        // schema change would otherwise surface as «no such column» halfway
        // through a run. No migrations before 1.0: the base is rebuilt from
        // the export in minutes.
        let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if found != 0 && found != SCHEMA_VERSION {
            bail!(
                "the database is on schema v{found}, the code expects v{SCHEMA_VERSION}. \
                 Delete {} and re-ingest",
                path.display()
            );
        }

        conn.execute_batch(SCHEMA)
            .context("could not apply the schema")?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;

        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    /// `access_hash` is never overwritten with null: the export does not know
    /// it, MTProto does, and it must survive either order of ingest.
    pub fn upsert_dialog(&self, d: &Dialog) -> Result<()> {
        self.conn.execute(
            "INSERT INTO dialogs (id, kind, access_hash, title, username, archived, msg_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
               kind        = excluded.kind,
               access_hash = COALESCE(excluded.access_hash, dialogs.access_hash),
               title       = excluded.title,
               username    = COALESCE(excluded.username, dialogs.username),
               archived    = excluded.archived,
               msg_count   = MAX(excluded.msg_count, dialogs.msg_count)",
            params![
                d.id,
                d.kind.as_str(),
                d.access_hash,
                d.title,
                d.username,
                d.archived as i32,
                d.msg_count,
            ],
        )?;
        Ok(())
    }

    /// `INSERT OR REPLACE`: re-ingesting the same export is idempotent.
    pub fn insert_messages(&mut self, msgs: &[Message]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut n = 0;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO messages
                 (dialog_id, msg_id, date, from_id, outgoing, reply_to,
                  grouped_id, fwd_from, media_type, file_path, text)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            )?;
            for m in msgs {
                stmt.execute(params![
                    m.dialog_id,
                    m.msg_id,
                    m.date,
                    m.from_id,
                    m.outgoing as i32,
                    m.reply_to,
                    m.grouped_id,
                    m.fwd_from,
                    m.media_type,
                    m.file_path,
                    m.text,
                ])?;
                n += 1;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    pub fn insert_hits(&mut self, hits: &[Hit]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut n = 0;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO hits
                 (dialog_id, msg_id, axis, rule_id, term, surface, layer, tier, priority, protected)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            )?;
            for h in hits {
                stmt.execute(params![
                    h.dialog_id,
                    h.msg_id,
                    h.axis.as_str(),
                    h.rule_id,
                    h.term,
                    h.surface,
                    h.layer.as_str(),
                    h.tier.as_str(),
                    h.priority,
                    h.protected as i32,
                ])?;
                n += 1;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Mark the protected. Set by the scan over every message, including
    /// those the dictionary never fired on.
    pub fn mark_protected(&mut self, dialog_id: i64, ids: &[i32]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE messages SET protected = 1 WHERE dialog_id = ?1 AND msg_id = ?2",
            )?;
            for id in ids {
                stmt.execute(params![dialog_id, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Drop the results of a scan. The corpus itself stays: rescanning must
    /// not require re-ingesting.
    pub fn clear_hits(&self) -> Result<()> {
        self.conn.execute("DELETE FROM hits", [])?;
        self.conn.execute("UPDATE messages SET protected = 0", [])?;
        Ok(())
    }
}
