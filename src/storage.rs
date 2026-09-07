use anyhow::Result;
use chrono::Utc;
use parking_lot::Mutex;
use rusqlite::{params, Connection, ToSql};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;


#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct MemoryItem {
    pub id: String,
    pub key: String,       // e.g. "theme_preference", "user_email"
    pub value: String,     // e.g. "dark mode", "user@example.com"
    pub category: String,  // "preference", "credential", "fact"
    pub status: String,    // "active", "pending_confirmation", "discarded"
    pub confidence: f64,
    pub created_at: String,
    pub updated_at: String,
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

            -- Graph edges between concepts/memories
            CREATE TABLE IF NOT EXISTS graph_edges (
                id TEXT PRIMARY KEY,
                source_node TEXT NOT NULL,
                target_node TEXT NOT NULL,
                relation TEXT NOT NULL,
                weight REAL NOT NULL,
                created_at TEXT NOT NULL,
                UNIQUE(source_node, target_node, relation)
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
            "#,
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn insert_artifact(
        &self,
        session_id: &str,
        artifact_type: &str,
        content: &str,
        metadata: &str,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO artifacts (id, session_id, artifact_type, content, metadata, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, session_id, artifact_type, content, metadata, now],
        )?;
        Ok(id)
    }

    pub fn get_active_memories(&self) -> Result<Vec<MemoryItem>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, key, value, category, status, confidence, created_at, updated_at FROM memories WHERE status = 'active' ORDER BY updated_at DESC LIMIT 20",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(MemoryItem {
                id: row.get(0)?,
                key: row.get(1)?,
                value: row.get(2)?,
                category: row.get(3)?,
                status: row.get(4)?,
                confidence: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?;
        let mut items = Vec::new();
        for r in rows {
            items.push(r?);
        }
        Ok(items)
    }

    /// Prefilter: keyword hit on key/value across up to 8 query words, capped for the LLM filter.
    /// Falls back to top-confidence active memories when no keyword matches, so the LLM filter
    /// still sees candidates instead of an empty list.
    pub fn search_memories(&self, query_words: &[String], limit: usize) -> Result<Vec<MemoryItem>> {
        let conn = self.conn.lock();
        if query_words.is_empty() {
            drop(conn);
            return self.get_active_memories();
        }
        // WHERE ... AND (key LIKE ?1 OR value LIKE ?1 OR ... OR key LIKE ?N OR value LIKE ?N) LIMIT ?N+1
        let words: Vec<String> = query_words.iter().take(8).map(|w| format!("%{}%", w.to_lowercase())).collect();
        let conds: Vec<String> = (1..=words.len())
            .map(|i| format!("key LIKE ?{i} OR value LIKE ?{i}"))
            .collect();
        let sql = format!(
            "SELECT id, key, value, category, status, confidence, created_at, updated_at
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
                id: row.get(0)?,
                key: row.get(1)?,
                value: row.get(2)?,
                category: row.get(3)?,
                status: row.get(4)?,
                confidence: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?;
        let mut items = Vec::new();
        for r in rows {
            items.push(r?);
        }
        if items.is_empty() {
            drop(items);
            drop(stmt);
            drop(conn);
            return self.get_active_memories();
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
        let conn = self.conn.lock();
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

    pub fn insert_graph_edge(
        &self,
        source_node: &str,
        target_node: &str,
        relation: &str,
        weight: f64,
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO graph_edges (id, source_node, target_node, relation, weight, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(source_node, target_node, relation) DO UPDATE SET
                weight = weight + 0.1
            "#,
            params![id, source_node, target_node, relation, weight, now],
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
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO pending_confirmations (id, session_id, key, value, category, prompt_question, status, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)",
            params![id, session_id, key, value, category, question, now],
        )?;
        Ok(id)
    }

    pub fn resolve_confirmation(&self, id_or_key: &str, confirmed: bool) -> Result<bool> {
        let (pending_id, key, value, category) = {
            let conn = self.conn.lock();
            let result = conn.query_row(
                "SELECT id, key, value, category FROM pending_confirmations WHERE (id = ?1 OR key = ?1) AND status = 'pending' LIMIT 1",
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
            let conn = self.conn.lock();
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
        let conn = self.conn.lock();
        let artifact_count: i64 = conn.query_row("SELECT count(*) FROM artifacts", [], |r| r.get(0))?;
        let memory_count: i64 = conn.query_row("SELECT count(*) FROM memories WHERE status = 'active'", [], |r| r.get(0))?;
        let edge_count: i64 = conn.query_row("SELECT count(*) FROM graph_edges", [], |r| r.get(0))?;
        let pending_count: i64 = conn.query_row("SELECT count(*) FROM pending_confirmations WHERE status = 'pending'", [], |r| r.get(0))?;

        Ok(serde_json::json!({
            "artifacts_stored": artifact_count,
            "active_memories": memory_count,
            "graph_edges": edge_count,
            "pending_confirmations": pending_count
        }))
    }
}
