//! P12-T02 seam 4: model settings and scope configuration. The model-role
//! settings accessors and the scope config repository move here byte for byte
//! from `src/storage.rs`. The child module still uses the private
//! `DbStore::run`/`read` helpers, so no visibility was widened.

use super::{now, scope_row, DbStore, ScopeConfig, ScopePatch};
use crate::safety;
use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

impl DbStore {
    pub async fn settings(&self) -> Result<Value> {
        self.run(|c| {
            let mut map = serde_json::Map::new();
            for role in ["main", "extraction", "compaction", "verification"] {
                let value: Option<String> = c
                    .query_row(
                        "SELECT value FROM settings WHERE key=?1",
                        [format!("model.{role}")],
                        |r| r.get(0),
                    )
                    .optional()?;
                map.insert(role.into(), json!(value.unwrap_or_default()));
            }
            Ok(Value::Object(map))
        })
        .await
    }
    pub async fn set_settings(
        &self,
        data: std::collections::BTreeMap<String, String>,
    ) -> Result<()> {
        for (role, value) in &data {
            if !["main", "extraction", "compaction", "verification"].contains(&role.as_str())
                || value.len() > 128
                || value.chars().any(char::is_control)
            {
                bail!("invalid model setting");
            }
        }
        self.run(move|c|{ let tx=c.transaction()?; for (role,value) in data {tx.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![format!("model.{role}"),value.trim()])?;} tx.commit()?; Ok(()) }).await
    }
    pub async fn role_model(&self, role: &str, default: &str) -> Result<String> {
        let key = format!("model.{role}");
        let default = default.to_string();
        self.run(move |c| {
            let v: Option<String> = c
                .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                    r.get(0)
                })
                .optional()?;
            Ok(v.filter(|s| !s.is_empty()).unwrap_or(default))
        })
        .await
    }
    /// The stored scope row, or `None` when the scope was never configured (API answers 404).
    pub async fn scope_config(&self, scope: String) -> Result<Option<ScopeConfig>> {
        self.run(move |c| {
            Ok(
                c.query_row(crate::agentic_sql::SCOPE_GET, [scope], scope_row)
                    .optional()?,
            )
        })
        .await
    }
    /// Merge a validated patch into the stored row so a partial POST never clears a column
    /// P1-T15: every configured scope, newest config included, for the UI's scope picker. A scope
    /// with `root_path: null` is listed too: it exists, it just cannot run tools yet.
    pub async fn scopes(&self) -> Result<Vec<ScopeConfig>> {
        self.read(move |c| {
            let mut stmt = c.prepare(crate::agentic_sql::SCOPES_LIST)?;
            let rows = stmt
                .query_map([], scope_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }
    /// Merge a validated patch into the stored row so a partial POST never clears a column
    /// the caller did not mention; `created_at` survives every later update.
    ///
    /// A patch that names no field asks for no change, so an existing row is returned untouched
    /// instead of being rewritten with a fresh `updated_at` — "last changed" then means it. A
    /// scope that does not exist yet is still created, because that is what posting to a new
    /// scope asks for.
    pub async fn upsert_scope(&self, scope: String, patch: ScopePatch) -> Result<ScopeConfig> {
        safety::scope(&scope)?;
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stored = tx
                .query_row(crate::agentic_sql::SCOPE_GET, [&scope], scope_row)
                .optional()?;
            let mut next = match stored {
                Some(unchanged) if patch.is_empty() => {
                    tx.commit()?;
                    return Ok(unchanged);
                }
                Some(row) => row,
                None => ScopeConfig::blank(&scope),
            };
            // `apply` is the only merge path: an `Unchanged` field cannot silently write.
            patch.root_path.apply(&mut next.root_path);
            if let Some(value) = patch.permission_mode {
                next.permission_mode = value;
            }
            patch.diagnostics_cmd.apply(&mut next.diagnostics_cmd);
            patch.max_steps.apply(&mut next.max_steps);
            patch.max_tool_bytes.apply(&mut next.max_tool_bytes);
            patch.max_wall_seconds.apply(&mut next.max_wall_seconds);
            tx.execute(
                crate::agentic_sql::SCOPE_UPSERT,
                params![
                    next.scope,
                    next.root_path,
                    next.permission_mode,
                    next.diagnostics_cmd,
                    next.max_steps,
                    next.max_tool_bytes,
                    next.max_wall_seconds,
                    now()
                ],
            )?;
            let stored = tx.query_row(crate::agentic_sql::SCOPE_GET, [&scope], scope_row)?;
            tx.commit()?;
            Ok(stored)
        })
        .await
    }
}
