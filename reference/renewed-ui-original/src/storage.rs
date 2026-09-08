use anyhow::Result;
use chrono::Utc;
use std::sync::{Arc, Mutex};
use rusqlite::{params, Connection, ToSql};
use uuid::Uuid;


/// One active-memory row, as consumed by the recall filter (key/value only).
#[derive(Clone, Debug)]
pub struct MemoryItem {
    pub key: String,   // e.g. "theme_preference", "user_email"
    pub value: String, // e.g. "dark mode", "user@example.com"
}


#[derive(Clone)]
pub struct DbStore {
    conn: Arc<Mutex<Connection>>,
}

impl DbStore {
    pub fn init(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;

            -- Raw untruncated artifact hub: prompts, tools, responses
            CREATE TABLE IF NOT EXISTS artifacts (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                artifact_type TEXT NOT NULL,
                content TEXT NOT NULL,
                metadata TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            -- Distilled long term memories (preferences, facts, credentials)
            CREATE TABLE IF NOT EXISTS memories (
                id TEXT PRIMARY KEY,
                key TEXT NOT NULL UNIQUE,
                value TEXT NOT NULL,
                category TEXT NOT NULL,
                status TEXT NOT NULL,
                confidence REAL NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );


            -- Pending confirmations for Gatekeeper (Agent 3)
            CREATE TABLE IF NOT EXISTS pending_confirmations (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                category TEXT NOT NULL,
                prompt_question TEXT NOT NULL,
                status TEXT NOT NULL, -- "pending", "confirmed", "rejected"
                created_at TEXT NOT NULL
            );

            -- Runtime configuration (per-role model overrides etc.)
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Get a settings value (settings table, KV).
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let v = conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |r| r.get::<_, String>(0),
        );
        Ok(v.ok())
    }

    /// Set a settings value (upsert).
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn insert_artifact(
        &self,
        session_id: &str,
        artifact_type: &str,
        content: &str,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO artifacts (id, session_id, artifact_type, content, metadata, created_at) VALUES (?1, ?2, ?3, ?4, '{}', ?5)",
            params![id, session_id, artifact_type, content, now],
        )?;
        Ok(id)
    }

    /// Prefilter: keyword hit on key/value across up to 8 query words, capped for the LLM filter.
    /// No hits → empty; recall_context skips the LLM filter call in that case.
    pub fn search_memories(&self, query_words: &[String], limit: usize) -> Result<Vec<MemoryItem>> {
        if query_words.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // WHERE ... AND (key LIKE ?1 OR value LIKE ?1 OR ... OR key LIKE ?N OR value LIKE ?N) LIMIT ?N+1
        let words: Vec<String> = query_words.iter().take(8).map(|w| format!("%{}%", w.to_lowercase())).collect();
        let conds: Vec<String> = (1..=words.len())
            .map(|i| format!("key LIKE ?{i} OR value LIKE ?{i}"))
            .collect();
        let sql = format!(
            "SELECT key, value
             FROM memories
             WHERE status = 'active' AND ({})
             ORDER BY confidence DESC, updated_at DESC
             LIMIT ?{}",
            conds.join(" OR "),
            words.len() + 1
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut bind: Vec<&dyn ToSql> = words.iter().map(|w| w as &dyn ToSql).collect();
        let limit_i = limit as i64;
        bind.push(&limit_i);
        let rows = stmt.query_map(bind.as_slice(), |row| {
            Ok(MemoryItem {
                key: row.get(0)?,
                value: row.get(1)?,
            })
        })?;
        let mut items = Vec::new();
        for r in rows {
            items.push(r?);
        }
        Ok(items)
    }

    pub fn upsert_memory(
        &self,
        key: &str,
        value: &str,
        category: &str,
        status: &str,
        confidence: f64,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let id = Uuid::new_v4().to_string();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            r#"
            INSERT INTO memories (id, key, value, category, status, confidence, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                category = excluded.category,
                status = excluded.status,
                confidence = excluded.confidence,
                updated_at = excluded.updated_at
            "#,
            params![id, key, value, category, status, confidence, now],
        )?;
        Ok(())
    }

    pub fn insert_pending_confirmation(
        &self,
        session_id: &str,
        key: &str,
        value: &str,
        category: &str,
        question: &str,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO pending_confirmations (id, session_id, key, value, category, prompt_question, status, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)",
            params![id, session_id, key, value, category, question, now],
        )?;
        Ok(id)
    }

    pub fn resolve_confirmation(&self, id_or_key: &str, confirmed: bool) -> Result<bool> {
        let (pending_id, key, value, category) = {
            let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
            let result = conn.query_row(
                "SELECT id, key, value, category FROM pending_confirmations WHERE id = ?1 AND status = 'pending' LIMIT 1",
                params![id_or_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            );

            match result {
                Ok(tuple) => tuple,
                Err(_) => return Ok(false),
            }
        };

        {
            let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
            let status = if confirmed { "confirmed" } else { "rejected" };
            conn.execute(
                "UPDATE pending_confirmations SET status = ?1 WHERE id = ?2",
                params![status, pending_id],
            )?;
        }

        if confirmed {
            self.upsert_memory(&key, &value, &category, "active", 1.0)?;
        }
        Ok(true)
    }

    pub fn get_stats(&self) -> Result<serde_json::Value> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let (artifact_count, memory_count, pending_count): (i64, i64, i64) = conn.query_row(
            "SELECT
                (SELECT count(*) FROM artifacts),
                (SELECT count(*) FROM memories WHERE status = 'active'),
                (SELECT count(*) FROM pending_confirmations WHERE status = 'pending')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;

        Ok(serde_json::json!({
            "artifacts_stored": artifact_count,
            "active_memories": memory_count,
            "pending_confirmations": pending_count
        }))
    }
}
