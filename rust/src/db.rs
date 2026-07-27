//! SQLite persistence (Process C: Memory & State).
//!
//! One connection, guarded by a mutex, in WAL mode. Android can kill the process
//! at any point, so every write is committed immediately rather than batched at
//! shutdown — "serialise on app close" is a best-effort optimisation, not the
//! durability story.

use std::path::Path;

use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::{Result, VonError};

static DB: OnceCell<Mutex<Connection>> = OnceCell::new();

pub fn init(db_path: &Path) -> Result<()> {
    if DB.get().is_some() {
        return Ok(());
    }
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(db_path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // Bound SQLite's page cache; on a low-end device this competes with the KV
    // cache of the local model.
    conn.pragma_update(None, "cache_size", -8000)?;
    migrate(&conn)?;
    DB.set(Mutex::new(conn))
        .map_err(|_| VonError::Db("database already initialised".into()))?;
    Ok(())
}

fn with_db<T>(f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let cell = DB
        .get()
        .ok_or_else(|| VonError::Db("database not initialised; call init_app first".into()))?;
    let conn = cell.lock();
    f(&conn)
}

const SCHEMA_VERSION: i32 = 1;

fn migrate(conn: &Connection) -> Result<()> {
    let current: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current >= SCHEMA_VERSION {
        return Ok(());
    }
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS projects (
            id           TEXT PRIMARY KEY,
            -- The user-facing VON number ("VON 3"). Monotonic and never
            -- reused: deleting VON 2 leaves a gap, which is correct — the
            -- number is an identity, not a position, and renumbering the
            -- survivors would rename projects the user already knows.
            ordinal      INTEGER NOT NULL DEFAULT 1,
            name         TEXT NOT NULL,
            description  TEXT NOT NULL DEFAULT '',
            kind         TEXT NOT NULL DEFAULT 'general',
            provider     TEXT NOT NULL DEFAULT 'local',
            model_id     TEXT NOT NULL DEFAULT '',
            created_at   INTEGER NOT NULL,
            updated_at   INTEGER NOT NULL,
            size_bytes   INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS messages (
            id         TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            seq        INTEGER NOT NULL,
            role       TEXT NOT NULL,
            content    TEXT NOT NULL,
            steps_json TEXT NOT NULL DEFAULT '[]',
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_messages_project
            ON messages(project_id, seq);

        -- Long-term memory: the serialised context window plus any rolling
        -- summary produced by context-window shifting.
        CREATE TABLE IF NOT EXISTS project_memory (
            project_id     TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
            context_json   TEXT NOT NULL DEFAULT '[]',
            summary        TEXT NOT NULL DEFAULT '',
            token_estimate INTEGER NOT NULL DEFAULT 0,
            updated_at     INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS settings (
            k TEXT PRIMARY KEY,
            v TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS api_keys (
            provider   TEXT PRIMARY KEY,
            key        TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS local_models (
            id              TEXT PRIMARY KEY,
            name            TEXT NOT NULL,
            url             TEXT NOT NULL,
            quant           TEXT NOT NULL DEFAULT '',
            size_bytes      INTEGER NOT NULL DEFAULT 0,
            ram_required_mb INTEGER NOT NULL DEFAULT 0,
            context_len     INTEGER NOT NULL DEFAULT 4096,
            file_path       TEXT NOT NULL DEFAULT '',
            downloaded      INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS device_profile (
            id           INTEGER PRIMARY KEY CHECK (id = 1),
            tier         INTEGER NOT NULL,
            cores        INTEGER NOT NULL,
            total_ram_mb INTEGER NOT NULL,
            score        REAL NOT NULL,
            measured_at  INTEGER NOT NULL
        );
        "#,
    )?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ---- projects -----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRow {
    pub id: String,
    /// The user-facing VON number. See the schema comment on `projects.ordinal`.
    pub ordinal: i64,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub provider: String,
    pub model_id: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub size_bytes: i64,
}

/// The number the next VON should carry.
///
/// `MAX + 1` rather than `COUNT + 1`: counting would reuse the number of a
/// deleted VON and silently rename an existing one.
pub fn next_ordinal() -> Result<i64> {
    with_db(|c| {
        let max: Option<i64> =
            c.query_row("SELECT MAX(ordinal) FROM projects", [], |r| r.get(0))?;
        Ok(max.unwrap_or(0) + 1)
    })
}

pub fn insert_project(p: &ProjectRow) -> Result<()> {
    with_db(|c| {
        c.execute(
            "INSERT INTO projects
             (id, ordinal, name, description, kind, provider, model_id, created_at,
              updated_at, size_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                p.id,
                p.ordinal,
                p.name,
                p.description,
                p.kind,
                p.provider,
                p.model_id,
                p.created_at,
                p.updated_at,
                p.size_bytes
            ],
        )?;
        Ok(())
    })
}

const PROJECT_COLUMNS: &str = "id, ordinal, name, description, kind, provider, model_id, \
                               created_at, updated_at, size_bytes";

pub fn list_projects() -> Result<Vec<ProjectRow>> {
    with_db(|c| {
        let mut stmt = c.prepare(&format!(
            "SELECT {PROJECT_COLUMNS} FROM projects ORDER BY updated_at DESC"
        ))?;
        let rows = stmt
            .query_map([], map_project)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

pub fn get_project(id: &str) -> Result<ProjectRow> {
    with_db(|c| {
        c.query_row(
            &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE id = ?1"),
            params![id],
            map_project,
        )
        .optional()?
        .ok_or_else(|| VonError::NotFound(format!("VON '{id}'")))
    })
}

fn map_project(r: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectRow> {
    Ok(ProjectRow {
        id: r.get(0)?,
        ordinal: r.get(1)?,
        name: r.get(2)?,
        description: r.get(3)?,
        kind: r.get(4)?,
        provider: r.get(5)?,
        model_id: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
        size_bytes: r.get(9)?,
    })
}

pub fn update_project_model(id: &str, provider: &str, model_id: &str) -> Result<()> {
    with_db(|c| {
        c.execute(
            "UPDATE projects SET provider = ?2, model_id = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, provider, model_id, now_ms()],
        )?;
        Ok(())
    })
}

pub fn rename_project(id: &str, name: &str) -> Result<()> {
    with_db(|c| {
        c.execute(
            "UPDATE projects SET name = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, name, now_ms()],
        )?;
        Ok(())
    })
}

pub fn touch_project(id: &str, size_bytes: i64) -> Result<()> {
    with_db(|c| {
        c.execute(
            "UPDATE projects SET updated_at = ?2, size_bytes = ?3 WHERE id = ?1",
            params![id, now_ms(), size_bytes],
        )?;
        Ok(())
    })
}

pub fn delete_project(id: &str) -> Result<()> {
    with_db(|c| {
        c.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
        Ok(())
    })
}

// ---- messages -----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRow {
    pub id: String,
    pub project_id: String,
    pub seq: i64,
    pub role: String,
    pub content: String,
    pub steps_json: String,
    pub created_at: i64,
}

pub fn insert_message(m: &MessageRow) -> Result<()> {
    with_db(|c| {
        c.execute(
            "INSERT OR REPLACE INTO messages
             (id, project_id, seq, role, content, steps_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                m.id,
                m.project_id,
                m.seq,
                m.role,
                m.content,
                m.steps_json,
                m.created_at
            ],
        )?;
        Ok(())
    })
}

pub fn list_messages(project_id: &str) -> Result<Vec<MessageRow>> {
    with_db(|c| {
        let mut stmt = c.prepare(
            "SELECT id, project_id, seq, role, content, steps_json, created_at
             FROM messages WHERE project_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt
            .query_map(params![project_id], |r| {
                Ok(MessageRow {
                    id: r.get(0)?,
                    project_id: r.get(1)?,
                    seq: r.get(2)?,
                    role: r.get(3)?,
                    content: r.get(4)?,
                    steps_json: r.get(5)?,
                    created_at: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

pub fn next_seq(project_id: &str) -> Result<i64> {
    with_db(|c| {
        let max: Option<i64> = c.query_row(
            "SELECT MAX(seq) FROM messages WHERE project_id = ?1",
            params![project_id],
            |r| r.get(0),
        )?;
        Ok(max.unwrap_or(-1) + 1)
    })
}

pub fn clear_messages(project_id: &str) -> Result<()> {
    with_db(|c| {
        c.execute("DELETE FROM messages WHERE project_id = ?1", params![project_id])?;
        c.execute(
            "DELETE FROM project_memory WHERE project_id = ?1",
            params![project_id],
        )?;
        Ok(())
    })
}

// ---- long-term memory ---------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryRow {
    pub context_json: String,
    pub summary: String,
    pub token_estimate: i64,
}

pub fn save_memory(project_id: &str, mem: &MemoryRow) -> Result<()> {
    with_db(|c| {
        c.execute(
            "INSERT INTO project_memory
                (project_id, context_json, summary, token_estimate, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project_id) DO UPDATE SET
                context_json   = excluded.context_json,
                summary        = excluded.summary,
                token_estimate = excluded.token_estimate,
                updated_at     = excluded.updated_at",
            params![
                project_id,
                mem.context_json,
                mem.summary,
                mem.token_estimate,
                now_ms()
            ],
        )?;
        Ok(())
    })
}

pub fn load_memory(project_id: &str) -> Result<MemoryRow> {
    with_db(|c| {
        Ok(c.query_row(
            "SELECT context_json, summary, token_estimate
             FROM project_memory WHERE project_id = ?1",
            params![project_id],
            |r| {
                Ok(MemoryRow {
                    context_json: r.get(0)?,
                    summary: r.get(1)?,
                    token_estimate: r.get(2)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
    })
}

// ---- settings & keys ----------------------------------------------------

pub fn set_setting(k: &str, v: &str) -> Result<()> {
    with_db(|c| {
        c.execute(
            "INSERT INTO settings (k, v) VALUES (?1, ?2)
             ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            params![k, v],
        )?;
        Ok(())
    })
}

pub fn get_setting(k: &str) -> Result<Option<String>> {
    with_db(|c| {
        Ok(c.query_row("SELECT v FROM settings WHERE k = ?1", params![k], |r| {
            r.get(0)
        })
        .optional()?)
    })
}

pub fn set_api_key(provider: &str, key: &str) -> Result<()> {
    with_db(|c| {
        if key.is_empty() {
            c.execute("DELETE FROM api_keys WHERE provider = ?1", params![provider])?;
        } else {
            c.execute(
                "INSERT INTO api_keys (provider, key, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(provider) DO UPDATE SET
                    key = excluded.key, updated_at = excluded.updated_at",
                params![provider, key, now_ms()],
            )?;
        }
        Ok(())
    })
}

pub fn get_api_key(provider: &str) -> Result<Option<String>> {
    with_db(|c| {
        Ok(c.query_row(
            "SELECT key FROM api_keys WHERE provider = ?1",
            params![provider],
            |r| r.get(0),
        )
        .optional()?)
    })
}

pub fn providers_with_keys() -> Result<Vec<String>> {
    with_db(|c| {
        let mut stmt = c.prepare("SELECT provider FROM api_keys ORDER BY provider")?;
        // Collect into a local before returning: `Ok(stmt.query_map(..)...)`
        // keeps the borrow of `stmt` alive inside the returned expression,
        // which outlives `stmt` itself.
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    })
}

// ---- local models -------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModelRow {
    pub id: String,
    pub name: String,
    pub url: String,
    pub quant: String,
    pub size_bytes: i64,
    pub ram_required_mb: i64,
    pub context_len: i64,
    pub file_path: String,
    pub downloaded: bool,
}

pub fn upsert_local_model(m: &LocalModelRow) -> Result<()> {
    with_db(|c| {
        c.execute(
            "INSERT INTO local_models
                (id, name, url, quant, size_bytes, ram_required_mb, context_len,
                 file_path, downloaded)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name, url = excluded.url, quant = excluded.quant,
                size_bytes = excluded.size_bytes,
                ram_required_mb = excluded.ram_required_mb,
                context_len = excluded.context_len,
                file_path = excluded.file_path,
                downloaded = excluded.downloaded",
            params![
                m.id,
                m.name,
                m.url,
                m.quant,
                m.size_bytes,
                m.ram_required_mb,
                m.context_len,
                m.file_path,
                m.downloaded as i32
            ],
        )?;
        Ok(())
    })
}

pub fn list_local_models() -> Result<Vec<LocalModelRow>> {
    with_db(|c| {
        let mut stmt = c.prepare(
            "SELECT id, name, url, quant, size_bytes, ram_required_mb, context_len,
                    file_path, downloaded
             FROM local_models ORDER BY size_bytes ASC",
        )?;
        // Collect into a local first — see the note in providers_with_keys.
        let rows = stmt
            .query_map([], |r| {
                Ok(LocalModelRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    url: r.get(2)?,
                    quant: r.get(3)?,
                    size_bytes: r.get(4)?,
                    ram_required_mb: r.get(5)?,
                    context_len: r.get(6)?,
                    file_path: r.get(7)?,
                    downloaded: r.get::<_, i32>(8)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

pub fn get_local_model(id: &str) -> Result<Option<LocalModelRow>> {
    Ok(list_local_models()?.into_iter().find(|m| m.id == id))
}

// ---- device profile -----------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DeviceProfileRow {
    pub tier: i32,
    pub cores: i32,
    pub total_ram_mb: i64,
    pub score: f64,
    pub measured_at: i64,
}

pub fn save_device_profile(p: &DeviceProfileRow) -> Result<()> {
    with_db(|c| {
        c.execute(
            "INSERT INTO device_profile (id, tier, cores, total_ram_mb, score, measured_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                tier = excluded.tier, cores = excluded.cores,
                total_ram_mb = excluded.total_ram_mb, score = excluded.score,
                measured_at = excluded.measured_at",
            params![p.tier, p.cores, p.total_ram_mb, p.score, p.measured_at],
        )?;
        Ok(())
    })
}

pub fn load_device_profile() -> Result<Option<DeviceProfileRow>> {
    with_db(|c| {
        Ok(c.query_row(
            "SELECT tier, cores, total_ram_mb, score, measured_at FROM device_profile WHERE id = 1",
            [],
            |r| {
                Ok(DeviceProfileRow {
                    tier: r.get(0)?,
                    cores: r.get(1)?,
                    total_ram_mb: r.get(2)?,
                    score: r.get(3)?,
                    measured_at: r.get(4)?,
                })
            },
        )
        .optional()?)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn setup() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let _ = init(&dir.path().join("test.db"));
        dir
    }

    #[test]
    #[serial]
    fn project_round_trip() {
        let _d = setup();
        let p = ProjectRow {
            id: "p1".into(),
            ordinal: 7,
            name: "Sales Dashboard".into(),
            description: String::new(),
            kind: "general".into(),
            provider: "local".into(),
            model_id: "phi-3-mini".into(),
            created_at: 1,
            updated_at: 1,
            size_bytes: 0,
        };
        let _ = delete_project("p1");
        insert_project(&p).unwrap();
        assert_eq!(get_project("p1").unwrap().name, "Sales Dashboard");
        assert_eq!(get_project("p1").unwrap().ordinal, 7);
        rename_project("p1", "Renamed").unwrap();
        assert_eq!(get_project("p1").unwrap().name, "Renamed");
        // Renaming must not renumber: "VON 7" is the user's handle for it.
        assert_eq!(get_project("p1").unwrap().ordinal, 7);
        delete_project("p1").unwrap();
        assert!(get_project("p1").is_err());
    }

    #[test]
    #[serial]
    fn ordinals_are_monotonic_and_never_reused() {
        let _d = setup();
        for id in ["o1", "o2", "o3"] {
            let _ = delete_project(id);
        }

        let make = |id: &str, ordinal: i64| ProjectRow {
            id: id.into(),
            ordinal,
            name: id.into(),
            description: String::new(),
            kind: "general".into(),
            provider: "local".into(),
            model_id: String::new(),
            created_at: 1,
            updated_at: 1,
            size_bytes: 0,
        };

        let first = next_ordinal().unwrap();
        insert_project(&make("o1", first)).unwrap();
        let second = next_ordinal().unwrap();
        insert_project(&make("o2", second)).unwrap();
        assert_eq!(second, first + 1);

        // Deleting the newest VON must not hand its number to the next one —
        // otherwise two different projects share a name in the user's head.
        delete_project("o2").unwrap();
        let third = next_ordinal().unwrap();
        insert_project(&make("o3", third)).unwrap();
        assert_eq!(third, second + 1, "a deleted ordinal must leave a gap");

        for id in ["o1", "o3"] {
            delete_project(id).unwrap();
        }
    }

    #[test]
    #[serial]
    fn messages_cascade_with_project() {
        let _d = setup();
        let _ = delete_project("p2");
        insert_project(&ProjectRow {
            id: "p2".into(),
            ordinal: 1,
            name: "x".into(),
            description: String::new(),
            kind: "general".into(),
            provider: "local".into(),
            model_id: String::new(),
            created_at: 1,
            updated_at: 1,
            size_bytes: 0,
        })
        .unwrap();
        insert_message(&MessageRow {
            id: "m1".into(),
            project_id: "p2".into(),
            seq: 0,
            role: "user".into(),
            content: "hi".into(),
            steps_json: "[]".into(),
            created_at: 1,
        })
        .unwrap();
        assert_eq!(list_messages("p2").unwrap().len(), 1);
        assert_eq!(next_seq("p2").unwrap(), 1);
        delete_project("p2").unwrap();
        assert!(list_messages("p2").unwrap().is_empty());
    }
}
