use crate::{
    ingest::Event,
    memory_agents::{ModelUsage, SpendLimits},
    safety,
};
use anyhow::{bail, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;
use uuid::Uuid;

pub fn now() -> String {
    Utc::now().to_rfc3339()
}
pub fn uid() -> String {
    Uuid::new_v4().to_string()
}
#[derive(Clone)]
pub struct DbStore {
    conn: Arc<Mutex<Connection>>,
    read_conns: Arc<Mutex<VecDeque<Connection>>>,
    pub commit_notify: tokio::sync::broadcast::Sender<()>,
    permits: Arc<Semaphore>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proposal {
    pub key: String,
    pub value: String,
    pub category: String,
    pub evidence_id: String,
    pub quote: String,
    #[serde(default)]
    pub priority: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recall {
    pub id: String,
    pub scope: String,
    pub key: String,
    pub value: String,
    pub revision: i64,
    pub evidence: Value,
}
pub struct Job {
    pub id: String,
    pub scope: String,
    pub source_id: String,
    pub events: Vec<Event>,
    pub attempts: i64,
}

// Exercised by this module's tests. Production edge inserts go through
// agent_loop::steps raw SQL, where migration 006 CHECK constraints enforce the same
// kinds, relations, id lengths and self-edge ban at the database layer.
#[allow(dead_code)]
pub const PROVENANCE_NODE_KINDS: [&str; 6] = [
    "evidence",
    "step",
    "permission",
    "mutation",
    "memory",
    "recovery",
];
#[allow(dead_code)]
pub const PROVENANCE_RELATIONS: [&str; 7] = [
    "supports",
    "contradicts",
    "depends_on",
    "authorizes",
    "mutates",
    "invalidates",
    "triggers",
];

#[allow(dead_code)]
fn validate_provenance_edge(
    source_kind: &str,
    source_id: &str,
    relation: &str,
    target_kind: &str,
    target_id: &str,
) -> Result<()> {
    if !PROVENANCE_NODE_KINDS.contains(&source_kind)
        || !PROVENANCE_NODE_KINDS.contains(&target_kind)
    {
        bail!("unsupported provenance node kind");
    }
    if !PROVENANCE_RELATIONS.contains(&relation) {
        bail!("unsupported provenance relation");
    }
    if source_id.is_empty()
        || source_id.len() > 128
        || target_id.is_empty()
        || target_id.len() > 128
    {
        bail!("provenance row ids must contain 1..128 bytes");
    }
    if source_kind == target_kind && source_id == target_id {
        bail!("a provenance edge cannot point to itself");
    }
    Ok(())
}

// ---- Scopes (P1-T04) ----------------------------------------------------------------
// A scope owns a project: docs/design/agentic-turn.md#scopes. `root_path` stays NULL until
// the owner points the scope at a directory, and every tool refuses until it is set.
pub const DEFAULT_MAX_STEPS: i64 = 40;
pub const DEFAULT_MAX_TOOL_BYTES: i64 = 400_000;
pub const DEFAULT_MAX_WALL_SECONDS: i64 = 900;

/// Plan limits from docs/design/tools.md#todo_write, mirroring the 003 CHECK constraints.
/// `todo_write` reads them so the tool and the schema can never disagree.
pub const MAX_PLAN_ITEMS: usize = 30;
pub const MAX_PLAN_TEXT: usize = 200;
pub const PLAN_STATUSES: [&str; 4] = ["pending", "in_progress", "done", "failed"];

/// Preview cap for the step API (P1-T12), matching the `substr(...,1,2048)` in `STEPS_LIST`.
/// Kept next to that constant's only reader so the two cannot drift.
pub const PREVIEW_BYTES: usize = 2048;
const MAX_VERIFICATION_PROJECTION_CLAIMS: usize = 20;
const MAX_VERIFICATION_PROJECTION_EVIDENCE_IDS: usize = 8;
const MAX_VERIFICATION_PROJECTION_DIAGNOSTICS: usize = 10;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScopeConfig {
    pub scope: String,
    pub root_path: Option<String>,
    pub permission_mode: String,
    pub diagnostics_cmd: Option<String>,
    pub max_steps: Option<i64>,
    pub max_tool_bytes: Option<i64>,
    pub max_wall_seconds: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

impl ScopeConfig {
    /// Unsaved defaults for a scope that has never been configured.
    pub fn blank(scope: &str) -> Self {
        Self {
            scope: scope.into(),
            root_path: None,
            permission_mode: "ask".into(),
            diagnostics_cmd: None,
            max_steps: None,
            max_tool_bytes: None,
            max_wall_seconds: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
    /// An unreadable stored value degrades to the safest mode instead of failing the turn.
    pub fn mode(&self) -> crate::tools::PermissionMode {
        crate::tools::PermissionMode::parse(&self.permission_mode)
            .unwrap_or(crate::tools::PermissionMode::Ask)
    }
    /// `(max_steps, max_tool_bytes, max_wall_seconds)` with the design defaults applied.
    pub fn budgets(&self) -> (i64, i64, i64) {
        (
            self.max_steps.unwrap_or(DEFAULT_MAX_STEPS),
            self.max_tool_bytes.unwrap_or(DEFAULT_MAX_TOOL_BYTES),
            self.max_wall_seconds.unwrap_or(DEFAULT_MAX_WALL_SECONDS),
        )
    }
    /// `None` means "chat only". The loop hands it to `Registry::invoke`, which then refuses
    /// every call instead of guessing a working directory.
    pub fn tool_ctx(&self, request_id: &str, step_id: &str) -> Option<crate::tools::ToolCtx> {
        let root = self.root_path.as_ref()?;
        Some(crate::tools::ToolCtx {
            root: root.into(),
            scope: self.scope.clone(),
            request_id: request_id.into(),
            step_id: step_id.into(),
            diagnostics_cmd: self.diagnostics_cmd.clone(),
        })
    }
}

fn scope_row(r: &rusqlite::Row) -> rusqlite::Result<ScopeConfig> {
    Ok(ScopeConfig {
        scope: r.get(0)?,
        root_path: r.get(1)?,
        permission_mode: r.get(2)?,
        diagnostics_cmd: r.get(3)?,
        max_steps: r.get(4)?,
        max_tool_bytes: r.get(5)?,
        max_wall_seconds: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

/// An absent field leaves the stored column alone; an explicit `null` clears it.
/// The session plan in `seq` order. Shared by `plan` and `write_plan` so the value the tool
/// returns to the model is read back from the same rows the UI will show.
fn plan_rows(c: &Connection, session_id: &str) -> Result<Value> {
    let mut stmt = c.prepare(crate::agentic_sql::PLAN_LIST)?;
    let rows = stmt.query_map([session_id], |r| {
        Ok(json!({
            "seq": r.get::<_, i64>(0)?, "text": r.get::<_, String>(1)?,
            "status": r.get::<_, String>(2)?, "updated_at": r.get::<_, String>(3)?,
        }))
    })?;
    Ok(json!({ "items": rows.collect::<rusqlite::Result<Vec<_>>>()? }))
}

fn bounded_projection_text(value: Option<&str>, max: usize) -> String {
    value.unwrap_or_default().chars().take(max).collect()
}

fn verification_projection(
    id: String,
    step_status: String,
    output: Option<String>,
    error_code: Option<String>,
    finished_at: Option<String>,
    projection_capped: bool,
) -> Value {
    let parsed = output
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or(Value::Null);
    let reported_status = parsed
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| {
            matches!(
                *status,
                "verified" | "unverified" | "skipped" | "unavailable"
            )
        });
    let status = if projection_capped {
        "unavailable"
    } else {
        reported_status.unwrap_or("unavailable")
    };
    let claims = if projection_capped {
        Vec::new()
    } else {
        parsed.get("claims").and_then(Value::as_array).into_iter().flatten()
        .take(MAX_VERIFICATION_PROJECTION_CLAIMS).filter_map(|claim|{
            let claim_text=claim.get("claim")?.as_str()?;
            let claim_status=claim.get("status")?.as_str()?;
            if !matches!(claim_status,"verified"|"unverified") {return None;}
            let evidence_step_ids=claim.get("evidence_step_ids").and_then(Value::as_array).into_iter().flatten()
                .filter_map(Value::as_str).take(MAX_VERIFICATION_PROJECTION_EVIDENCE_IDS)
                .map(|id|bounded_projection_text(Some(id),128)).collect::<Vec<_>>();
            Some(json!({"claim":bounded_projection_text(Some(claim_text),500),"status":claim_status,
                "evidence_step_ids":evidence_step_ids,
                "reason":bounded_projection_text(claim.get("reason").and_then(Value::as_str),500)}))
        }).collect::<Vec<_>>()
    };
    let skipped_diagnostics = if projection_capped {
        Vec::new()
    } else {
        parsed
            .get("skipped_diagnostics")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .take(MAX_VERIFICATION_PROJECTION_DIAGNOSTICS)
            .map(|item| bounded_projection_text(Some(item), 240))
            .collect::<Vec<_>>()
    };
    let unverified_claims = claims
        .iter()
        .filter(|claim| claim["status"] == "unverified")
        .count();
    json!({"step_id":id,"status":status,"step_status":step_status,"claims":claims,
        "unverified_claims":unverified_claims,"skipped_diagnostics":skipped_diagnostics,
        "model":bounded_projection_text(parsed.get("model").and_then(Value::as_str),128),
        "error_code":error_code,"finished_at":finished_at,"projection_capped":projection_capped})
}

/// The plan limits from docs/design/tools.md#todo_write, checked before SQLite so a CHECK
/// failure never reaches the caller as an opaque database error.
pub(crate) fn validate_plan(items: &[(String, String)]) -> Result<()> {
    if items.len() > MAX_PLAN_ITEMS {
        bail!("a plan holds at most {MAX_PLAN_ITEMS} items");
    }
    if items
        .iter()
        .any(|(text, _)| text.trim().is_empty() || text.trim().chars().count() > MAX_PLAN_TEXT)
    {
        bail!("every plan item needs 1..{MAX_PLAN_TEXT} characters of text");
    }
    if items
        .iter()
        .any(|(_, status)| !PLAN_STATUSES.contains(&status.as_str()))
    {
        bail!("unknown plan item status");
    }
    if items
        .iter()
        .filter(|(_, status)| status == "in_progress")
        .count()
        > 1
    {
        bail!("only one plan item may be in_progress");
    }
    Ok(())
}

/// Clear-and-insert inside the caller's transaction, returning the stored plan. The agent loop
/// calls this while finishing the step that produced the plan, so a plan and the step that
/// produced it become visible together or not at all.
pub(crate) fn write_plan(
    c: &Connection,
    session_id: &str,
    items: &[(String, String)],
) -> Result<Value> {
    validate_plan(items)?;
    c.execute(crate::agentic_sql::PLAN_CLEAR, [session_id])?;
    let stamp = now();
    for (i, (text, status)) in items.iter().enumerate() {
        c.execute(
            crate::agentic_sql::PLAN_INSERT,
            params![uid(), session_id, i as i64 + 1, text.trim(), status, stamp],
        )?;
    }
    plan_rows(c, session_id)
}

fn patch_field<'de, D, T>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(d).map(Some)
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopePatch {
    #[serde(default, deserialize_with = "patch_field")]
    pub root_path: Option<Option<String>>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default, deserialize_with = "patch_field")]
    pub diagnostics_cmd: Option<Option<String>>,
    #[serde(default, deserialize_with = "patch_field")]
    pub max_steps: Option<Option<i64>>,
    #[serde(default, deserialize_with = "patch_field")]
    pub max_tool_bytes: Option<Option<i64>>,
    #[serde(default, deserialize_with = "patch_field")]
    pub max_wall_seconds: Option<Option<i64>>,
}

impl ScopePatch {
    /// True when the caller named no field at all. An empty patch states no intent, so
    /// `upsert_scope` answers with the stored row instead of rewriting it, and `updated_at`
    /// moves only when a value actually moved.
    pub fn is_empty(&self) -> bool {
        self.root_path.is_none()
            && self.permission_mode.is_none()
            && self.diagnostics_cmd.is_none()
            && self.max_steps.is_none()
            && self.max_tool_bytes.is_none()
            && self.max_wall_seconds.is_none()
    }
    /// Normalize and reject before anything reaches SQLite, so a bad request is a 400 and
    /// never a CHECK-constraint failure. Canonicalizing `root_path` touches the filesystem.
    pub fn validate(mut self) -> std::result::Result<Self, &'static str> {
        if let Some(Some(raw)) = &self.root_path {
            let canonical = canonical_root(raw)?;
            self.root_path = Some(Some(canonical));
        }
        if let Some(mode) = self.permission_mode.as_deref() {
            if crate::tools::PermissionMode::parse(mode).is_none() {
                return Err("permission_mode must be ask, auto_edit or auto_all");
            }
        }
        if let Some(Some(raw)) = &self.diagnostics_cmd {
            let cmd = raw.trim().to_string();
            if cmd.is_empty() {
                self.diagnostics_cmd = Some(None);
            } else if cmd.chars().count() > 512 || cmd.chars().any(char::is_control) {
                return Err("diagnostics_cmd must be 1-512 characters without control characters");
            } else if crate::tools::is_dangerous_command(&cmd) {
                return Err("diagnostics_cmd matches the destructive-command deny-list");
            } else {
                self.diagnostics_cmd = Some(Some(cmd));
            }
        }
        bounded(
            self.max_steps,
            1,
            500,
            "max_steps must be between 1 and 500",
        )?;
        bounded(
            self.max_tool_bytes,
            1024,
            50_000_000,
            "max_tool_bytes must be between 1024 and 50000000",
        )?;
        bounded(
            self.max_wall_seconds,
            10,
            86_400,
            "max_wall_seconds must be between 10 and 86400",
        )?;
        Ok(self)
    }
}

fn bounded(
    value: Option<Option<i64>>,
    low: i64,
    high: i64,
    message: &'static str,
) -> std::result::Result<(), &'static str> {
    match value {
        Some(Some(n)) if !(low..=high).contains(&n) => Err(message),
        _ => Ok(()),
    }
}

/// `root_path` must be an absolute, existing directory outside the harness data directory
/// (which holds the database and is denied to every tool).
pub fn canonical_root(input: &str) -> std::result::Result<String, &'static str> {
    let raw = input.trim();
    if raw.is_empty() || raw.len() > 4096 || raw.chars().any(char::is_control) {
        return Err("root_path must be 1-4096 characters without control characters");
    }
    let requested = std::path::Path::new(raw);
    if !requested.is_absolute() {
        return Err("root_path must be an absolute path");
    }
    let canonical = std::fs::canonicalize(requested).map_err(|_| "root_path does not exist")?;
    if !canonical.is_dir() {
        return Err("root_path must be a directory");
    }
    if canonical.parent().is_none() {
        return Err("root_path must not be the filesystem root");
    }
    if crate::tools::paths::harness_data_dir().is_some_and(|data| canonical.starts_with(data)) {
        return Err("root_path must not be inside the harness data directory");
    }
    canonical
        .to_str()
        .map(str::to_string)
        .ok_or("root_path must be valid UTF-8")
}

impl DbStore {
    pub fn init(path: &str) -> Result<Self> {
        let (commit_notify, _) = tokio::sync::broadcast::channel(128);

        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Inspect first: rejecting a legacy DB must not change its journal mode.
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version == 0 {
            let existing:i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",[],|r|r.get(0))?;
            if existing != 0 {
                bail!(
                    "legacy or unknown database: use scripts/migrate_legacy.py into a NEW database"
                );
            }
        } else if !(1..=10).contains(&version) {
            bail!("unsupported schema version {version}");
        }
        conn.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )?;
        if version == 0 {
            conn.execute_batch(include_str!("../migrations/001_core.sql"))?;
        }
        if version < 2 {
            conn.execute_batch(include_str!("../migrations/002_recording.sql"))?;
        }
        if version < 3 {
            conn.execute_batch(include_str!("../migrations/003_agentic.sql"))?;
        }
        if version < 4 {
            conn.execute_batch(include_str!("../migrations/004_memory_kinds.sql"))?;
        }
        if version < 5 {
            conn.execute_batch(include_str!("../migrations/005_generation_stream.sql"))?;
        }
        if version < 6 {
            conn.execute_batch(include_str!("../migrations/006_provenance_edges.sql"))?;
        }
        if version < 7 {
            conn.execute_batch(include_str!("../migrations/007_privacy_archive.sql"))?;
        }
        if version < 8 {
            conn.execute_batch(include_str!("../migrations/008_provider_spend.sql"))?;
        }
        if version < 9 {
            conn.execute_batch(include_str!("../migrations/009_run_cancellation.sql"))?;
        }
        if version < 10 {
            conn.execute_batch(include_str!("../migrations/010_retention_maintenance.sql"))?;
        }
        conn.execute(
            "UPDATE provider_calls SET state='failed',usage_status='unavailable',reason='process_restarted_with_call_reserved',finished_at=?1 WHERE state='reserved'",
            [Utc::now().to_rfc3339()],
        )?;
        let foreign_key_errors: i64 =
            conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })?;
        if foreign_key_errors != 0 {
            bail!("schema migration left {foreign_key_errors} foreign-key errors");
        }
        // The executable acquires ProcessLock before init. Recovery therefore interrupts stale
        // work exactly once; a rejected contender never resets another process's billed work.
        crate::recording::recover(&mut conn)?;
        let notify = commit_notify.clone();
        conn.commit_hook(Some(move || {
            let _ = notify.send(());
            false
        }));
        let mut readers: VecDeque<Connection> = VecDeque::new();
        if path != ":memory:" {
            for _ in 0..4 {
                let reader = Connection::open(path)?;
                reader.busy_timeout(std::time::Duration::from_secs(5))?;
                reader.execute_batch(
                    "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;",
                )?;
                readers.push_back(reader);
            }
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            read_conns: Arc::new(Mutex::new(readers)),
            commit_notify,
            permits: Arc::new(Semaphore::new(32)),
        })
    }
    pub async fn run<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow::anyhow!("database queue full"))?;
        let shared = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut conn = shared
                .lock()
                .map_err(|_| anyhow::anyhow!("database lock poisoned"))?;
            f(&mut conn)
        })
        .await?
    }
    /// Execute bounded read projections on dedicated read-only connections.
    pub async fn read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow::anyhow!("database queue full"))?;
        let pool = self.read_conns.clone();
        let writer = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let conn = pool
                .lock()
                .map_err(|_| anyhow::anyhow!("database read pool lock poisoned"))?
                .pop_front();
            if let Some(conn) = conn {
                let result = f(&conn);
                pool.lock()
                    .map_err(|_| anyhow::anyhow!("database read pool lock poisoned"))?
                    .push_back(conn);
                result
            } else {
                let conn = writer
                    .lock()
                    .map_err(|_| anyhow::anyhow!("database lock poisoned"))?;
                f(&conn)
            }
        })
        .await?
    }
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
    /// Store a model-produced turn summary as review-only episodic evidence. It does not enter
    /// recall until a person approves the candidate.
    pub async fn save_compaction_candidate(
        &self,
        scope: String,
        request: String,
        step: String,
        summary: String,
    ) -> Result<String> {
        safety::validate_fact("turn_summary", &summary, "episodic")?;
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let source_id=format!("compaction:{step}");
            let fingerprint=crate::tools::content_hash(&summary);
            let stamp=now();
            tx.execute("INSERT INTO sources(id,scope,name,format,fingerprint,parser_version,content,warnings,created_at) VALUES(?1,?2,?3,'compaction',?4,'turn-compaction-v1',?5,'[]',?6) ON CONFLICT(id) DO NOTHING",
                params![source_id,scope,format!("Turn compaction {request}"),fingerprint,summary,stamp])?;
            let revision:i64=tx.query_row("SELECT revision FROM memories WHERE scope=?1 AND key='turn_summary'",[&scope],|r|r.get(0)).optional()?.unwrap_or(0);
            let candidate=uid();
            let evidence=json!({"source_id":source_id,"request_id":request,"step_id":step,"kind":"compaction","quote":summary});
            tx.execute("INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at) VALUES(?1,?2,'turn_summary',?3,'episodic',?4,?5,?6,'pending',?7,?8)",
                params![candidate,scope,summary,source_id,evidence.to_string(),revision,stamp,Utc::now().timestamp()+30*86400])?;
            tx.commit()?;
            Ok(candidate)
        }).await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn ingest(
        &self,
        scope: String,
        name: String,
        format: String,
        content: String,
        fingerprint: String,
        warnings: Vec<String>,
        chunks: Vec<Vec<Event>>,
    ) -> Result<Value> {
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let existing:Option<String>=tx.query_row("SELECT id FROM sources WHERE scope=?1 AND fingerprint=?2 AND parser_version=?3",params![scope,fingerprint,crate::ingest::PARSER_VERSION],|r|r.get(0)).optional()?;
            if let Some(id)=existing {return Ok(json!({"source_id":id,"duplicate":true,"chunks_queued":0}));}
            let queued:i64=tx.query_row("SELECT count(*) FROM jobs WHERE status IN ('pending','running')",[],|r|r.get(0))?;
            if queued+chunks.len() as i64>1000 {bail!("extraction queue is full");}
            let id=uid();
            tx.execute("INSERT INTO sources(id,scope,name,format,fingerprint,parser_version,content,warnings,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![id,scope,name,format,fingerprint,crate::ingest::PARSER_VERSION,content,serde_json::to_string(&warnings)?,now()])?;
            for (i,chunk) in chunks.iter().enumerate(){
                tx.execute("INSERT INTO jobs(id,job_key,scope,source_id,payload,status,available_at,created_at) VALUES(?1,?2,?3,?4,?5,'pending',?6,?7)",params![uid(),format!("{id}:chunk:{i}"),scope,id,serde_json::to_string(chunk)?,Utc::now().timestamp(),now()])?;
            }
            let result=json!({"source_id":id,"duplicate":false,"chunks_queued":chunks.len(),"warnings":warnings});
            tx.commit()?;Ok(result)
        }).await
    }
    pub async fn claim_job(&self) -> Result<Option<Job>> {
        self.flush_recording_outbox().await?;
        self.run(|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row:Option<(String,String,String,String,i64)>=tx.query_row("SELECT id,scope,source_id,payload,attempts FROM jobs WHERE status='pending' AND available_at<=?1 ORDER BY created_at,id LIMIT 1",[Utc::now().timestamp()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
            let Some((id,scope,source_id,payload,attempts))=row else{return Ok(None)};
            let events=serde_json::from_str(&payload)?;
            tx.execute("UPDATE jobs SET status='running',attempts=attempts+1 WHERE id=?1 AND status='pending'",[&id])?;
            tx.commit()?;Ok(Some(Job{id,scope,source_id,events,attempts:attempts+1}))
        }).await
    }
    pub async fn finish_job(&self, job: Job, proposals: Vec<Proposal>) -> Result<usize> {
        // Validate the entire result before storing ANY proposals.
        if proposals.len() > 10 {
            bail!("too many extraction proposals");
        }
        for p in &proposals {
            safety::validate_fact(&p.key, &p.value, &p.category)?;
            if p.priority
                .as_deref()
                .is_some_and(|value| !["normal", "high"].contains(&value))
            {
                bail!("proposal has invalid priority");
            }
            if p.quote.trim().is_empty()
                || p.quote.chars().count() > 1000
                || !job.events.iter().any(|e| {
                    e.id == p.evidence_id && e.role == "user" && e.content.contains(&p.quote)
                })
            {
                bail!("proposal has invalid user evidence");
            }
        }
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;let mut count=0;
            for p in proposals {
                let revision:i64=tx.query_row("SELECT revision FROM memories WHERE scope=?1 AND key=?2",params![job.scope,p.key],|r|r.get(0)).optional()?.unwrap_or(0);
                let correction=safety::is_correction(&p.quote);
                let evidence=json!({"source_id":job.source_id,"event_id":p.evidence_id,"quote":p.quote,"priority":if correction{"high"}else{"normal"},"correction":correction});
                count+=tx.execute("INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending',?9,?10) ON CONFLICT(scope,key,value,source_id) DO NOTHING",params![uid(),job.scope,p.key,p.value,p.category,job.source_id,evidence.to_string(),revision,now(),Utc::now().timestamp()+30*86400])?;
            }
            tx.execute("UPDATE jobs SET status='done',last_error=NULL WHERE id=?1",[job.id])?;
            tx.commit()?;Ok(count)
        }).await
    }
    pub async fn fail_job(&self, id: String, attempts: i64) -> Result<()> {
        self.run(move|c|{c.execute("UPDATE jobs SET status=?1,available_at=?2,last_error='extraction or validation failed; inspect provider configuration and retry' WHERE id=?3",params![if attempts>=3{"failed"}else{"pending"},Utc::now().timestamp()+30*attempts,id])?;Ok(())}).await
    }
    pub async fn retry_job(&self, id: String) -> Result<bool> {
        self.run(move|c|Ok(c.execute("UPDATE jobs SET status='pending',attempts=0,available_at=?1,last_error=NULL WHERE id=?2 AND status='failed'",params![Utc::now().timestamp(),id])?==1)).await
    }
    /// Unfiltered candidate feed. Kept as the narrow entry point beside `candidate_feed`.
    #[allow(dead_code)]
    pub async fn candidates(&self, scope: String) -> Result<Value> {
        self.candidate_feed(scope, None, false, false).await
    }
    pub async fn candidate_feed(
        &self,
        scope: String,
        request: Option<String>,
        imports_only: bool,
        chat_only: bool,
    ) -> Result<Value> {
        self.run(move|c|{
            c.execute("UPDATE candidates SET status='expired',resolved_at=?1 WHERE status='pending' AND expires_at<=?2",params![now(),Utc::now().timestamp()])?;
            let mut stmt=c.prepare("SELECT c.id,c.scope,c.key,c.value,c.category,c.evidence,c.expected_revision,m.value,c.source_id,c.created_at FROM candidates c LEFT JOIN memories m ON m.scope=c.scope AND m.key=c.key WHERE c.scope=?1 AND c.status='pending' ORDER BY c.created_at LIMIT 500")?;
            let raw=stmt.query_map([scope],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,i64>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,String>(8)?,r.get::<_,String>(9)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut items=Vec::new();
            for (id,row_scope,key,value,category,evidence_text,expected_revision,old_value,source_id,created_at) in raw {
                let evidence=serde_json::from_str::<Value>(&evidence_text).unwrap_or(Value::Null);
                let request_id=evidence.get("request_id").and_then(Value::as_str).map(str::to_string).or_else(||source_id.strip_prefix("chat:").map(str::to_string));
                if request.as_ref().is_some_and(|wanted|request_id.as_ref()!=Some(wanted)){continue;}
                if imports_only && request_id.is_some(){continue;}
                if chat_only && request_id.is_none(){continue;}
                let priority=evidence.get("priority").and_then(Value::as_str).unwrap_or("normal");
                items.push(json!({"id":id,"scope":row_scope,"key":key,"value":value,"category":category,"evidence":evidence,"expected_revision":expected_revision,"old_value":old_value,"request_id":request_id,"priority":priority,"created_at":created_at}));
            }
            items.sort_by(|a,b|(b["priority"]==json!("high")).cmp(&(a["priority"]==json!("high"))).then_with(||a["created_at"].as_str().cmp(&b["created_at"].as_str())).then_with(||a["id"].as_str().cmp(&b["id"].as_str())));
            items.truncate(100);Ok(json!({"candidates":items}))
        }).await
    }
    pub async fn edit_candidate(
        &self,
        id: String,
        scope: String,
        new_value: String,
    ) -> Result<String> {
        let value = new_value.trim().to_string();
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row:Option<(String,String,String,String,i64,String)>=tx.query_row("SELECT key,category,status,source_id,expires_at,evidence FROM candidates WHERE id=?1 AND scope=?2",params![id,scope],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
            let Some((key,category,status,source_id,expiry,evidence_text))=row else{return Ok("not_found".into())};
            if status!="pending"{return Ok("already_resolved".into());}
            if expiry<=Utc::now().timestamp(){tx.execute("UPDATE candidates SET status='expired',resolved_at=?1 WHERE id=?2",params![now(),id])?;tx.commit()?;return Ok("expired".into());}
            safety::validate_fact(&key,&value,&category)?;
            let duplicate:Option<i64>=tx.query_row("SELECT 1 FROM candidates WHERE scope=?1 AND key=?2 AND value=?3 AND source_id=?4 AND id<>?5",params![scope,key,value,source_id,id],|r|r.get(0)).optional()?;
            if duplicate.is_some(){return Ok("conflict".into());}
            let mut evidence=serde_json::from_str::<Value>(&evidence_text)?;
            let Some(object)=evidence.as_object_mut() else {bail!("candidate evidence must be an object")};
            object.insert("edited".into(),json!(true));
            tx.execute("UPDATE candidates SET value=?1,evidence=?2 WHERE id=?3 AND scope=?4 AND status='pending'",params![value,evidence.to_string(),id,scope])?;
            tx.commit()?;Ok("edited".into())
        }).await
    }
    pub async fn resolve(&self, id: String, scope: String, confirm: bool) -> Result<String> {
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row:Option<(String,String,String,i64,String,i64)>=tx.query_row("SELECT key,value,category,expected_revision,status,expires_at FROM candidates WHERE id=?1 AND scope=?2",params![id,scope],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
            let Some((key,value,category,expected,status,expiry))=row else{return Ok("not_found".into())};
            if status!="pending" {return Ok("already_resolved".into());}
            if expiry<=Utc::now().timestamp(){tx.execute("UPDATE candidates SET status='expired',resolved_at=?1 WHERE id=?2",params![now(),id])?;tx.commit()?;return Ok("expired".into());}
            if !confirm {tx.execute("UPDATE candidates SET status='rejected',resolved_at=?1 WHERE id=?2 AND status='pending'",params![now(),id])?;tx.commit()?;return Ok("rejected".into());}
            safety::validate_fact(&key,&value,&category)?;
            let current:Option<(String,i64,String)>=tx.query_row("SELECT id,revision,value FROM memories WHERE scope=?1 AND key=?2",params![scope,key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if current.as_ref().map(|m|m.1).unwrap_or(0)!=expected {
                tx.execute("UPDATE candidates SET status='conflict',resolved_at=?1 WHERE id=?2",params![now(),id])?;tx.commit()?;return Ok("conflict".into());
            }
            let memory_id=current.as_ref().map(|m|m.0.clone()).unwrap_or_else(uid);
            let old=current.map(|m|m.2);let stamp=now();
            tx.execute("INSERT INTO memories(id,scope,key,value,category,status,revision,candidate_id,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,'active',?6,?7,?8,?8) ON CONFLICT(scope,key) DO UPDATE SET value=excluded.value,category=excluded.category,status='active',revision=excluded.revision,candidate_id=excluded.candidate_id,updated_at=excluded.updated_at",params![memory_id,scope,key,value,category,expected+1,id,stamp])?;
            tx.execute("INSERT INTO memory_revisions(id,memory_id,revision,action,old_value,new_value,candidate_id,created_at) VALUES(?1,?2,?3,'approve',?4,?5,?6,?7)",params![uid(),memory_id,expected+1,old,value,id,stamp])?;
            tx.execute("UPDATE candidates SET status='approved',resolved_at=?1 WHERE id=?2 AND status='pending'",params![stamp,id])?;
            tx.commit()?;Ok("approved".into())
        }).await
    }
    /// Hybrid local recall: union lexical and vector top-20s, then rerank deterministically.
    /// The final context payload retains the original 6,000-byte hard ceiling.
    pub async fn recall(&self, scope: String, prompt: String) -> Result<Vec<Recall>> {
        let fts = safety::fts_query(&prompt);
        let query_vector = crate::embeddings::embed(&prompt);
        if fts.is_empty() && query_vector.iter().all(|value| *value == 0.0) {
            return Ok(Vec::new());
        }
        self.run(move|c|{
            struct Row { memory:Recall, updated_at:String, vector:Vec<f32>, recall_count:i64, useful_count:i64 }
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let raw={
                let mut stmt=tx.prepare("SELECT m.id,m.scope,m.key,m.value,m.revision,c.evidence,m.updated_at,e.dimensions,e.vector,e.content_hash,COALESCE(e.recall_count,0),COALESCE(e.useful_count,0) FROM memories m JOIN candidates c ON c.id=m.candidate_id LEFT JOIN memory_embeddings e ON e.memory_id=m.id AND e.model=?1 WHERE m.status='active' AND (m.scope=?2 OR m.scope='global') AND (m.scope=?2 OR NOT EXISTS(SELECT 1 FROM memories p WHERE p.scope=?2 AND p.key=m.key AND p.status='active')) ORDER BY m.id LIMIT 10000")?;
                let collected=stmt.query_map(params![crate::embeddings::MODEL,scope],|r|Ok((
                    r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,
                    r.get::<_,Option<i64>>(7)?,r.get::<_,Option<Vec<u8>>>(8)?,r.get::<_,Option<String>>(9)?,r.get::<_,i64>(10)?,r.get::<_,i64>(11)?
                )))?.collect::<rusqlite::Result<Vec<_>>>()?;
                collected
            };
            let mut rows=Vec::new();
            for (id,row_scope,key,value,revision,evidence,updated_at,dimensions,blob,stored_hash,recall_count,useful_count) in raw {
                if safety::sensitive(&value){continue;}
                let content=format!("{key}\n{value}");
                let content_hash=safety::fingerprint(&content);
                let decoded=blob.as_deref().and_then(|bytes|crate::embeddings::decode(bytes,dimensions.unwrap_or_default() as usize));
                let cache_hit=stored_hash.as_deref()==Some(&content_hash) && dimensions==Some(crate::embeddings::DIMENSIONS as i64) && decoded.is_some();
                let vector=match decoded{Some(cached) if cache_hit=>cached,_=>crate::embeddings::embed(&content)};
                if !cache_hit {
                    tx.execute("INSERT INTO memory_embeddings(memory_id,model,dimensions,vector,content_hash,recall_count,useful_count,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(memory_id) DO UPDATE SET model=excluded.model,dimensions=excluded.dimensions,vector=excluded.vector,content_hash=excluded.content_hash,updated_at=excluded.updated_at",
                        params![id,crate::embeddings::MODEL,crate::embeddings::DIMENSIONS as i64,crate::embeddings::encode(&vector),content_hash,recall_count,useful_count,now()])?;
                }
                rows.push(Row{memory:Recall{id,scope:row_scope,key,value,revision,evidence:serde_json::from_str(&evidence).unwrap_or(Value::Null)},updated_at,vector,recall_count,useful_count});
            }

            let mut lexical=HashMap::<String,usize>::new();
            if !fts.is_empty(){
                let mut stmt=tx.prepare("SELECT m.id FROM memory_fts JOIN memories m ON m.rowid=memory_fts.rowid WHERE memory_fts MATCH ?1 AND m.status='active' AND (m.scope=?2 OR m.scope='global') AND (m.scope=?2 OR NOT EXISTS(SELECT 1 FROM memories p WHERE p.scope=?2 AND p.key=m.key AND p.status='active')) ORDER BY bm25(memory_fts),m.updated_at DESC LIMIT 20")?;
                for (rank,id) in stmt.query_map(params![fts,scope],|r|r.get::<_,String>(0))?.enumerate(){lexical.insert(id?,rank);}
            }
            let mut semantic=rows.iter().map(|row|(row.memory.id.clone(),crate::embeddings::cosine(&query_vector,&row.vector))).filter(|(_,score)|*score>0.01).collect::<Vec<_>>();
            semantic.sort_by(|a,b|b.1.total_cmp(&a.1).then_with(||a.0.cmp(&b.0)));semantic.truncate(20);
            let semantic=semantic.into_iter().collect::<HashMap<_,_>>();
            let candidate_ids=lexical.keys().chain(semantic.keys()).cloned().collect::<HashSet<_>>();
            let now_at=Utc::now();
            let mut ranked=rows.into_iter().filter(|row|candidate_ids.contains(&row.memory.id)).map(|row|{
                let lexical_score=lexical.get(&row.memory.id).map(|rank|2.0/(1.0+*rank as f64)).unwrap_or(0.0);
                let semantic_score=semantic.get(&row.memory.id).copied().unwrap_or(0.0).max(0.0) as f64;
                let scope_score=if row.memory.scope==scope{0.5}else{0.0};
                let days=chrono::DateTime::parse_from_rfc3339(&row.updated_at).map(|at|(now_at-at.with_timezone(&Utc)).num_days().max(0) as f64).unwrap_or(365.0);
                let recency_score=0.25/(1.0+days/30.0);
                let useful_ratio=row.useful_count.max(0) as f64/(1+row.recall_count.max(0)) as f64;
                (lexical_score+semantic_score+scope_score+recency_score+0.5*useful_ratio,row)
            }).collect::<Vec<_>>();
            ranked.sort_by(|a,b|b.0.total_cmp(&a.0).then_with(||b.1.updated_at.cmp(&a.1.updated_at)).then_with(||a.1.memory.id.cmp(&b.1.memory.id)));
            let mut out=Vec::new();let mut bytes=0;
            for (_,row) in ranked.into_iter().take(20){
                let size=serde_json::to_vec(&row.memory)?.len();if bytes+size>6000{continue;}bytes+=size;
                tx.execute("UPDATE memory_embeddings SET recall_count=recall_count+1 WHERE memory_id=?1",[&row.memory.id])?;
                out.push(row.memory);
            }
            tx.commit()?;Ok(out)
        }).await
    }

    pub async fn stats(&self) -> Result<Value> {
        self.read(|c|{
            let active:i64=c.query_row("SELECT count(*) FROM memories WHERE status='active'",[],|r|r.get(0))?;
            let pending:i64=c.query_row("SELECT count(*) FROM candidates WHERE status='pending' AND expires_at>?1",[Utc::now().timestamp()],|r|r.get(0))?;
            let sources:i64=c.query_row("SELECT count(*) FROM sources",[],|r|r.get(0))?;
            let queued:i64=c.query_row("SELECT count(*) FROM jobs WHERE status IN ('pending','running')",[],|r|r.get(0))?;
            let failed:i64=c.query_row("SELECT count(*) FROM jobs WHERE status='failed'",[],|r|r.get(0))?;
            Ok(json!({"active_memories":active,"pending_confirmations":pending,"sources_stored":sources,"queued_jobs":queued,"failed_jobs":failed}))
        }).await
    }
    pub async fn readiness(&self) -> Result<Value> {
        self.read(|c| {
            let schema_version:i64=c.query_row("PRAGMA user_version",[],|r|r.get(0))?;
            let quick_check:String=c.query_row("PRAGMA quick_check(1)",[],|r|r.get(0))?;
            let pending_jobs:i64=c.query_row("SELECT count(*) FROM jobs WHERE status='pending'",[],|r|r.get(0))?;
            let running_jobs:i64=c.query_row("SELECT count(*) FROM jobs WHERE status='running'",[],|r|r.get(0))?;
            let failed_jobs:i64=c.query_row("SELECT count(*) FROM jobs WHERE status='failed'",[],|r|r.get(0))?;
            let waiting_turns:i64=c.query_row("SELECT count(*) FROM chat_receipts WHERE state='captured'",[],|r|r.get(0))?;
            let running_turns:i64=c.query_row("SELECT count(*) FROM chat_receipts WHERE state='generating'",[],|r|r.get(0))?;
            let journal_mode:String=c.query_row("PRAGMA journal_mode",[],|r|r.get(0))?;
            let last_checkpoint:Option<String>=c.query_row("SELECT finished_at FROM maintenance_runs WHERE action='wal_checkpoint' ORDER BY id DESC LIMIT 1",[],|r|r.get(0)).optional()?;
            let last_retention:Option<String>=c.query_row("SELECT finished_at FROM maintenance_runs WHERE action='retention' ORDER BY id DESC LIMIT 1",[],|r|r.get(0)).optional()?;
            let last_compaction:Option<String>=c.query_row("SELECT finished_at FROM maintenance_runs WHERE action='compaction' ORDER BY id DESC LIMIT 1",[],|r|r.get(0)).optional()?;
            Ok(json!({"ready":schema_version==10&&quick_check=="ok","schema_version":schema_version,
                "quick_check":quick_check,"queue":{"jobs_pending":pending_jobs,"jobs_running":running_jobs,
                "jobs_failed":failed_jobs,"turns_waiting":waiting_turns,"turns_running":running_turns},
                "maintenance":{"journal_mode":journal_mode,"last_wal_checkpoint_at":last_checkpoint,
                "last_retention_at":last_retention,"last_compaction_at":last_compaction}}))
        }).await
    }
    /// Retention only ever targets derived, replayable rows. Receipts, provenance edges,
    /// memories and privacy/archive rows are never deletable by maintenance, and a turn is
    /// eligible only once its receipt reached a terminal state.
    // P13 retention/maintenance surface. Implemented and covered by this module's tests,
    // but no HTTP route calls it yet, so the binary build sees it as unreachable. Retained
    // deliberately rather than deleted; exposing it is a routing change, not a cleanup.
    #[allow(dead_code)]
    pub const RETENTION_TARGETS: [&'static str; 2] = ["generation_chunks", "activity_events"];

    #[allow(dead_code)]
    pub async fn retention_policies(&self) -> Result<Value> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT name,keep_days,enabled,updated_at FROM retention_policies ORDER BY name",
            )?;
            let policies = stmt
                .query_map([], |r| {
                    Ok(json!({"name":r.get::<_,String>(0)?,"keep_days":r.get::<_,i64>(1)?,
                        "enabled":r.get::<_,i64>(2)?==1,"updated_at":r.get::<_,String>(3)?}))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({ "policies": policies }))
        })
        .await
    }

    /// Configure one retention window. Unknown targets are refused so a typo can never be
    /// interpreted as permission to delete receipts.
    #[allow(dead_code)]
    pub async fn set_retention_policy(
        &self,
        name: String,
        keep_days: i64,
        enabled: bool,
    ) -> Result<Value> {
        if !Self::RETENTION_TARGETS.contains(&name.as_str()) {
            bail!("unknown retention target {name}");
        }
        if keep_days < 1 {
            bail!("keep_days must be at least 1");
        }
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "INSERT INTO retention_policies(name,keep_days,enabled,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(name) DO UPDATE SET keep_days=excluded.keep_days,enabled=excluded.enabled,updated_at=excluded.updated_at",
                params![name, keep_days, i64::from(enabled), now()],
            )?;
            tx.commit()?;
            Ok(json!({"name":name,"keep_days":keep_days,"enabled":enabled}))
        })
        .await
    }

    /// Collapse the chunk rows of finished turns into a single row that still replays the same
    /// text. Terminal rows and receipts are untouched, and live turns are skipped entirely.
    #[allow(dead_code)]
    pub async fn compact_generation_chunks(&self, older_than_days: i64) -> Result<Value> {
        let cutoff = (Utc::now() - chrono::Duration::days(older_than_days.max(0))).to_rfc3339();
        self.run(move |c| {
            let started = now();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let requests: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT g.request_id FROM generation_events g JOIN chat_receipts r ON r.request_id=g.request_id \
                     WHERE g.state='chunk' AND g.created_at<?1 AND r.state NOT IN ('captured','generating') \
                     GROUP BY g.request_id HAVING count(*)>1",
                )?;
                let collected = stmt
                    .query_map([&cutoff], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                collected
            };
            let mut compacted = 0i64;
            let mut removed = 0i64;
            for request_id in &requests {
                let rows: Vec<(i64, String)> = {
                    let mut stmt = tx.prepare(
                        "SELECT seq,content FROM generation_events WHERE request_id=?1 AND state='chunk' ORDER BY seq",
                    )?;
                    let collected = stmt
                        .query_map([request_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    collected
                };
                if rows.len() < 2 {
                    continue;
                }
                let keep = rows[0].0;
                let merged: String = rows.iter().map(|(_, content)| content.as_str()).collect();
                tx.execute(
                    "UPDATE generation_events SET content=?2,compacted_chunks=?3 WHERE seq=?1",
                    params![keep, merged, rows.len() as i64],
                )?;
                removed += tx.execute(
                    "DELETE FROM generation_events WHERE request_id=?1 AND state='chunk' AND seq<>?2",
                    params![request_id, keep],
                )? as i64;
                compacted += 1;
            }
            tx.execute(
                "INSERT INTO maintenance_runs(action,target,rows_affected,detail,started_at,finished_at) VALUES('compaction','generation_chunks',?1,?2,?3,?4)",
                params![removed, format!("{compacted} request(s) compacted"), started, now()],
            )?;
            tx.commit()?;
            Ok(json!({"requests_compacted":compacted,"chunk_rows_removed":removed,"cutoff":cutoff}))
        })
        .await
    }

    /// Apply every enabled retention policy. Disabled policies delete nothing, and each applied
    /// policy leaves an evidence row in `maintenance_runs`.
    #[allow(dead_code)]
    pub async fn apply_retention(&self) -> Result<Value> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let policies: Vec<(String, i64, i64)> = {
                let mut stmt = tx.prepare(
                    "SELECT name,keep_days,enabled FROM retention_policies ORDER BY name",
                )?;
                let collected = stmt
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                collected
            };
            let mut report = serde_json::Map::new();
            for (name, keep_days, enabled) in policies {
                if enabled != 1 {
                    report.insert(name, json!({"status":"disabled","rows_deleted":0}));
                    continue;
                }
                let started = now();
                let cutoff = (Utc::now() - chrono::Duration::days(keep_days)).to_rfc3339();
                let deleted = match name.as_str() {
                    "generation_chunks" => tx.execute(
                        "DELETE FROM generation_events WHERE state='chunk' AND created_at<?1 \
                         AND request_id IN (SELECT request_id FROM chat_receipts WHERE state NOT IN ('captured','generating')) \
                         AND request_id IN (SELECT request_id FROM generation_events WHERE state IN ('completed','interrupted','failed'))",
                        [&cutoff],
                    )?,
                    "activity_events" => tx.execute(
                        "DELETE FROM activity_events WHERE created_at<?1 \
                         AND request_id IN (SELECT request_id FROM chat_receipts WHERE state NOT IN ('captured','generating'))",
                        [&cutoff],
                    )?,
                    other => bail!("unknown retention target {other}"),
                } as i64;
                tx.execute(
                    "INSERT INTO maintenance_runs(action,target,rows_affected,detail,started_at,finished_at) VALUES('retention',?1,?2,?3,?4,?5)",
                    params![name, deleted, format!("keep_days={keep_days}"), started, now()],
                )?;
                report.insert(
                    name,
                    json!({"status":"applied","rows_deleted":deleted,"cutoff":cutoff}),
                );
            }
            tx.commit()?;
            Ok(Value::Object(report))
        })
        .await
    }

    /// WAL checkpoint monitoring plus optimize/analyze and incremental vacuum. Incremental
    /// vacuum is reported as unavailable rather than silently skipped when auto_vacuum is off.
    #[allow(dead_code)]
    pub async fn maintenance(&self) -> Result<Value> {
        self.run(|c| {
            let started = now();
            let journal_mode: String = c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
            let (busy, wal_pages, checkpointed): (i64, i64, i64) = c
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .unwrap_or((0, -1, -1));
            c.execute_batch("PRAGMA optimize; ANALYZE;")?;
            let auto_vacuum: i64 = c.query_row("PRAGMA auto_vacuum", [], |r| r.get(0))?;
            let freelist_before: i64 = c.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
            let vacuum = if auto_vacuum == 2 {
                c.execute_batch("PRAGMA incremental_vacuum;")?;
                "incremental_vacuum_ran"
            } else {
                "incremental_vacuum_unavailable_auto_vacuum_off"
            };
            let freelist_after: i64 = c.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
            c.execute(
                "INSERT INTO maintenance_runs(action,target,rows_affected,wal_pages,checkpointed_pages,freelist_pages,detail,started_at,finished_at) VALUES('wal_checkpoint','database',0,?1,?2,?3,?4,?5,?6)",
                params![
                    wal_pages,
                    checkpointed,
                    freelist_after,
                    format!("journal_mode={journal_mode}; busy={busy}; {vacuum}"),
                    started,
                    now()
                ],
            )?;
            Ok(json!({"journal_mode":journal_mode,"busy":busy,"wal_pages":wal_pages,
                "checkpointed_pages":checkpointed,"freelist_before":freelist_before,
                "freelist_after":freelist_after,"incremental_vacuum":vacuum}))
        })
        .await
    }
    pub async fn jobs(&self) -> Result<Value> {
        self.read(|c|{let mut stmt=c.prepare("SELECT id,scope,source_id,status,attempts,last_error FROM jobs ORDER BY created_at DESC LIMIT 100")?;
            let rows=stmt.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"scope":r.get::<_,String>(1)?,"source_id":r.get::<_,String>(2)?,"status":r.get::<_,String>(3)?,"attempts":r.get::<_,i64>(4)?,"error":r.get::<_,Option<String>>(5)?})))?;
            Ok(json!({"jobs":rows.collect::<rusqlite::Result<Vec<_>>>()?}))
        }).await
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
            if let Some(value) = patch.root_path {
                next.root_path = value;
            }
            if let Some(value) = patch.permission_mode {
                next.permission_mode = value;
            }
            if let Some(value) = patch.diagnostics_cmd {
                next.diagnostics_cmd = value;
            }
            if let Some(value) = patch.max_steps {
                next.max_steps = value;
            }
            if let Some(value) = patch.max_tool_bytes {
                next.max_tool_bytes = value;
            }
            if let Some(value) = patch.max_wall_seconds {
                next.max_wall_seconds = value;
            }
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
    /// The stored plan, for the `plan_updated` event and the UI's plan panel.
    pub async fn plan(&self, session_id: String) -> Result<Value> {
        self.read(move |c| plan_rows(c, &session_id)).await
    }

    // ---- P1-T12 read side: what the UI polls between turns ----------------------------
    // These only read rows the loop already committed. Nothing here recomputes a summary or
    // re-renders a diff, so the UI can never show a version of the turn the record disagrees with.

    /// Every step of one turn in `seq` order. `input_preview`/`output_preview` are the first
    /// 2 KB of the stored JSON as text (`SQL substr`), so a long tool output or a whole message
    /// array cannot blow up a poll; `previews_capped` says when that cut happened, because a
    /// chopped JSON string that pretends to be complete is worse than no preview at all.
    /// `summary` is the tool's own phrase, read back from the finished step's output; a step that
    /// is still running has none yet, and its `tool_started` activity event carries it instead.
    pub async fn turn_steps(&self, request_id: String) -> Result<Value> {
        self.run(move|c|{
            let mut stmt=c.prepare(crate::agentic_sql::STEPS_LIST)?;
            let rows=stmt.query_map([&request_id],|r|{
                let input:Option<String>=r.get(6)?;
                let output:Option<String>=r.get(7)?;
                let capped=[input.as_deref(),output.as_deref()].iter().flatten().any(|p|p.len()>=PREVIEW_BYTES);
                let summary=output.as_deref().and_then(|p|serde_json::from_str::<Value>(p).ok())
                    .and_then(|v|v.get("summary").and_then(Value::as_str).map(str::to_string));
                Ok(json!({
                    "id":r.get::<_,String>(0)?,"seq":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?,
                    "status":r.get::<_,String>(3)?,"tool_name":r.get::<_,Option<String>>(4)?,
                    "tool_call_id":r.get::<_,Option<String>>(5)?,"summary":summary,
                    "input_preview":input,"output_preview":output,"previews_capped":capped,
                    "output_bytes":r.get::<_,i64>(8)?,"truncated":r.get::<_,i64>(9)?==1,
                    "tokens_in":r.get::<_,Option<i64>>(10)?,"tokens_out":r.get::<_,Option<i64>>(11)?,
                    "error_code":r.get::<_,Option<String>>(12)?,
                    "started_at":r.get::<_,String>(13)?,"finished_at":r.get::<_,Option<String>>(14)?,
                }))
            })?.collect::<rusqlite::Result<Vec<_>>>()?;
            let verification=c.query_row(crate::agentic_sql::VERIFICATION_LATEST,[&request_id],|r|Ok(verification_projection(
                r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,
                r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,i64>(5)?==1,
            ))).optional()?;
            Ok(json!({"steps":rows,"verification":verification}))
        }).await
    }

    /// Atomically reserve one provider dispatch after checking persisted per-turn and UTC-day
    /// request, token, and cost totals. Refusals are durable and are never sent to the provider.
    pub async fn reserve_provider_call(
        &self,
        request_id: Option<String>,
        kind: String,
        model: String,
        limits: SpendLimits,
    ) -> Result<String> {
        let call_id = uid();
        let returned = call_id.clone();
        let day_start = format!("{}T00:00:00+00:00", Utc::now().date_naive());
        let decision = self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let totals = |request: Option<&str>| -> Result<(i64,i64,i64,i64)> {
                let (where_sql, request_param) = if request.is_some() {
                    ("created_at>=?1 AND request_id=?2", request)
                } else {
                    ("created_at>=?1", None)
                };
                let sql = format!("SELECT count(*),COALESCE(sum(prompt_tokens+completion_tokens),0),COALESCE(sum(cost_microusd),0),COALESCE(sum(CASE WHEN state IN ('complete','failed') AND (usage_status!='reported') THEN 1 ELSE 0 END),0) FROM provider_calls WHERE state IN ('reserved','complete','failed') AND {where_sql}");
                if let Some(value)=request_param {
                    Ok(tx.query_row(&sql, params![day_start,value], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?)
                } else {
                    Ok(tx.query_row(&sql, params![day_start], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?)
                }
            };
            let day=totals(None)?;
            let turn=if let Some(request)=request_id.as_deref(){totals(Some(request))?}else{(0,0,0,0)};
            let reason = if turn.0 >= limits.requests_per_turn as i64 { Some("turn_request_limit") }
                else if day.0 >= limits.requests_per_day as i64 { Some("daily_request_limit") }
                else if turn.3 > 0 && (limits.tokens_per_turn.is_some() || limits.cost_microusd_per_turn.is_some()) { Some("turn_usage_unavailable") }
                else if day.3 > 0 && (limits.tokens_per_day.is_some() || limits.cost_microusd_per_day.is_some()) { Some("daily_usage_unavailable") }
                else if limits.tokens_per_turn.is_some_and(|v| turn.1 >= v as i64) { Some("turn_token_limit") }
                else if limits.tokens_per_day.is_some_and(|v| day.1 >= v as i64) { Some("daily_token_limit") }
                else if limits.cost_microusd_per_turn.is_some_and(|v| turn.2 >= v as i64) { Some("turn_cost_limit") }
                else if limits.cost_microusd_per_day.is_some_and(|v| day.2 >= v as i64) { Some("daily_cost_limit") }
                else if (limits.cost_microusd_per_turn.is_some() || limits.cost_microusd_per_day.is_some()) && !limits.has_pricing() { Some("cost_pricing_unavailable") }
                else { None };
            let stamp=now();
            if let Some(reason)=reason {
                tx.execute("INSERT INTO provider_calls(call_id,request_id,kind,model,state,usage_status,reason,created_at,finished_at) VALUES(?1,?2,?3,?4,'refused','unavailable',?5,?6,?6)",params![call_id,request_id,kind,model,reason,stamp])?;
                if let Some(request)=request_id.as_deref() {
                    if let Some(session)=tx.query_row("SELECT session_id FROM chat_receipts WHERE request_id=?1",[request],|r|r.get::<_,String>(0)).optional()? {
                        tx.execute(crate::agentic_sql::EVENT,params![request,session,None::<String>,"provider_spend_refused",json!({"reason":reason}).to_string(),stamp])?;
                    }
                }
                tx.commit()?;
                return Ok(Err(reason.to_string()));
            }
            tx.execute("INSERT INTO provider_calls(call_id,request_id,kind,model,state,usage_status,created_at) VALUES(?1,?2,?3,?4,'reserved','pending',?5)",params![call_id,request_id,kind,model,stamp])?;
            tx.commit()?;
            Ok(Ok(()))
        }).await?;
        match decision {
            Ok(()) => Ok(returned),
            Err(reason) => bail!("provider spend refused: {reason}"),
        }
    }

    pub async fn finish_provider_call(
        &self,
        call_id: String,
        usage: ModelUsage,
        limits: SpendLimits,
        failed_reason: Option<String>,
    ) -> Result<()> {
        let prompt = usage.prompt_tokens.map(|v| v as i64);
        let completion = usage.completion_tokens.map(|v| v as i64);
        let reported = prompt.is_some() && completion.is_some();
        let cost = if reported {
            limits.cost_for(&usage).map(|v| v as i64)
        } else {
            None
        };
        let failed = failed_reason.is_some();
        let reason = failed_reason
            .or_else(|| usage.unavailable_reason.clone())
            .or_else(|| {
                if cost.is_none() {
                    Some("cost_pricing_unavailable".into())
                } else {
                    None
                }
            });
        self.run(move|c|{
            if c.execute("UPDATE provider_calls SET state=?2,prompt_tokens=?3,completion_tokens=?4,cost_microusd=?5,usage_status=?6,reason=?7,finished_at=?8 WHERE call_id=?1 AND state='reserved'",params![call_id,if failed{"failed"}else{"complete"},prompt,completion,cost,if reported{"reported"}else{"unavailable"},reason,now()])?!=1 { bail!("provider call reservation is not active"); }
            Ok(())
        }).await
    }

    /// Durable assistant generation chunks. The producer writes these rows before transport.
    pub async fn append_generation(
        &self,
        request_id: String,
        session_id: String,
        state: String,
        content: String,
        error_code: Option<String>,
    ) -> Result<i64> {
        self.run(move|c|{
            c.execute("INSERT INTO generation_events(request_id,session_id,state,content,error_code,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![request_id,session_id,state,content,error_code,now()])?;
            Ok(c.last_insert_rowid())
        }).await
    }

    pub async fn generation_since(&self, session_id: String, after_seq: i64) -> Result<Value> {
        self.run(move|c|{
            let mut stmt=c.prepare("SELECT seq,request_id,state,content,error_code,created_at FROM generation_events WHERE session_id=?1 AND seq>?2 ORDER BY seq LIMIT 200")?;
            let rows=stmt.query_map(params![session_id,after_seq],|r|Ok(json!({
                "seq":r.get::<_,i64>(0)?,"request_id":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"content":r.get::<_,String>(3)?,"error_code":r.get::<_,Option<String>>(4)?,"created_at":r.get::<_,String>(5)?
            })))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let next=rows.last().and_then(|e|e["seq"].as_i64()).unwrap_or(after_seq);
            Ok(json!({"events":rows,"next_after_seq":next}))
        }).await
    }

    /// The session's activity feed after `after_seq`, capped at 200 rows by `EVENTS_AFTER`.
    /// `next_after_seq` is the cursor to send back; it only moves when rows were returned, so a
    /// poll that finds nothing cannot skip an event that commits a moment later.
    pub async fn activity_since(&self, session_id: String, after_seq: i64) -> Result<Value> {
        self.read(move|c|{
            let mut stmt=c.prepare(crate::agentic_sql::EVENTS_AFTER)?;
            let rows=stmt.query_map(params![session_id,after_seq],|r|Ok(json!({
                "seq":r.get::<_,i64>(0)?,"request_id":r.get::<_,String>(1)?,"step_id":r.get::<_,Option<String>>(2)?,
                "kind":r.get::<_,String>(3)?,
                "payload":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(Value::Null),
                "created_at":r.get::<_,String>(5)?,
            })))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let next=rows.last().and_then(|e|e["seq"].as_i64()).unwrap_or(after_seq);
            Ok(json!({"events":rows,"next_after_seq":next}))
        }).await
    }

    /// The file changes one turn applied, with the diff the tool produced. `applied` is the row's
    /// own word: the loop writes it when the file was already written, never in advance.
    pub async fn turn_changes(&self, request_id: String) -> Result<Value> {
        self.run(move|c|{
            let mut stmt=c.prepare(crate::agentic_sql::FILE_CHANGES_LIST)?;
            let rows=stmt.query_map([request_id],|r|Ok(json!({
                "id":r.get::<_,String>(0)?,"step_id":r.get::<_,String>(1)?,"path":r.get::<_,String>(2)?,
                "action":r.get::<_,String>(3)?,"before_hash":r.get::<_,Option<String>>(4)?,
                "after_hash":r.get::<_,Option<String>>(5)?,"diff":r.get::<_,Option<String>>(6)?,
                "applied":r.get::<_,i64>(7)?==1,"reverted_at":r.get::<_,Option<String>>(8)?,
                "created_at":r.get::<_,String>(9)?,
            })))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({"changes":rows}))
        }).await
    }

    /// Persist one inspectable dependency between durable rows. There is deliberately no freeform
    /// reasoning payload: provenance says which rows relate and how, not what the model thought.
    #[allow(dead_code)]
    pub async fn record_provenance_edge(
        &self,
        request_id: String,
        source_kind: String,
        source_id: String,
        relation: String,
        target_kind: String,
        target_id: String,
    ) -> Result<String> {
        validate_provenance_edge(
            &source_kind,
            &source_id,
            &relation,
            &target_kind,
            &target_id,
        )?;
        self.run(move |c| {
            let id = uid();
            c.execute(
                crate::agentic_sql::PROVENANCE_EDGE_INSERT,
                params![
                    id,
                    request_id,
                    source_kind,
                    source_id,
                    relation,
                    target_kind,
                    target_id,
                    now()
                ],
            )?;
            Ok(id)
        })
        .await
    }

    #[allow(dead_code)]
    pub async fn provenance_edges(&self, request_id: String) -> Result<Value> {
        self.run(move |c| {
            let mut stmt = c.prepare(crate::agentic_sql::PROVENANCE_EDGES_LIST)?;
            let rows = stmt
                .query_map([request_id], |r| {
                    Ok(json!({
                        "id":r.get::<_,String>(0)?,"source_kind":r.get::<_,String>(1)?,
                        "source_id":r.get::<_,String>(2)?,"relation":r.get::<_,String>(3)?,
                        "target_kind":r.get::<_,String>(4)?,"target_id":r.get::<_,String>(5)?,
                        "created_at":r.get::<_,String>(6)?,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({"edges":rows}))
        })
        .await
    }

    /// Read-only incident projection. It joins bounded, externally inspectable rows and marks
    /// rows without a provenance edge as unknown instead of inventing causal support.
    pub async fn incident_graph(&self, request_id: String) -> Result<Option<Value>> {
        self.run(move |c| {
            let receipt: Option<(String, String, String, String, Option<String>, String)> = c
                .query_row(
                    "SELECT request_id,session_id,scope,state,error_code,updated_at FROM chat_receipts WHERE request_id=?1",
                    [&request_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
                .optional()?;
            let Some((request, session, scope, state, receipt_error, updated_at)) = receipt else {
                return Ok(None);
            };
            let mut nodes = Vec::<Value>::new();
            let mut node_ids = HashSet::<String>::new();
            let mut linked = HashSet::<String>::new();
            let mut upstream = HashMap::<String, Vec<String>>::new();
            let mut downstream = HashMap::<String, Vec<String>>::new();
            let mut breaks = Vec::<(i64, String, String, String)>::new();
            let node_id = |kind: &str, id: &str| format!("{kind}:{id}");
            let mut add = |kind: &str, id: &str, label: String, status: String, known: bool| {
                let full = node_id(kind, id);
                if node_ids.insert(full.clone()) {
                    nodes.push(json!({"id":full,"kind":kind,"row_id":id,"label":label,
                        "status":status,"provenance":if known {"known"} else {"unknown"}}));
                }
                full
            };
            add("request", &request, "coding turn".into(), state.clone(), true);
            if let Some(error) = receipt_error {
                // The receipt is the terminal summary. Prefer an earlier concrete failed or
                // interrupted step when one exists, rather than calling the summary the cause.
                breaks.push((30_000, "request".into(), request.clone(), error));
            }

            let mut stmt = c.prepare("SELECT id,seq,kind,status,COALESCE(tool_name,''),COALESCE(error_code,'') FROM turn_steps WHERE request_id=?1 ORDER BY seq LIMIT 200")?;
            for row in stmt.query_map([&request_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)))? {
                let (id, seq, kind, status, tool, error) = row?;
                let label = if tool.is_empty() { kind.clone() } else { format!("{kind}: {tool}") };
                add("step", &id, safety::redact(&label).chars().take(160).collect(), status.clone(), true);
                if matches!(status.as_str(), "failed" | "denied" | "interrupted") || !error.is_empty() {
                    breaks.push((seq + 1, "step".into(), id, if error.is_empty() { status } else { error }));
                }
            }
            let mut stmt = c.prepare("SELECT id,status,tool_name,summary FROM permission_requests WHERE request_id=?1 ORDER BY created_at LIMIT 200")?;
            for row in stmt.query_map([&request_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))? {
                let (id, status, tool, summary) = row?;
                add("permission", &id, safety::redact(&format!("{tool}: {summary}")).chars().take(240).collect(), status.clone(), true);
                if matches!(status.as_str(), "denied" | "expired") { breaks.push((10_000, "permission".into(), id, status)); }
            }
            let mut stmt = c.prepare("SELECT id,action,path,applied FROM file_changes WHERE request_id=?1 ORDER BY created_at LIMIT 200")?;
            for row in stmt.query_map([&request_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?)))? {
                let (id, action, path, applied) = row?;
                add("mutation", &id, safety::redact(&format!("{action} {path}")).chars().take(240).collect(), if applied == 1 { "applied" } else { "planned" }.into(), true);
            }
            let mut stmt = c.prepare("SELECT seq,kind FROM activity_events WHERE request_id=?1 ORDER BY seq LIMIT 200")?;
            for row in stmt.query_map([&request_id], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))? {
                let (seq, kind) = row?;
                if kind == "interrupted" {
                    let id = seq.to_string();
                    add("recovery", &id, "process interruption recovery".into(), "interrupted".into(), true);
                    breaks.push((20_000, "recovery".into(), id, "interrupted".into()));
                }
            }
            let mut edge_stmt = c.prepare(crate::agentic_sql::PROVENANCE_EDGES_LIST)?;
            let mut edges = Vec::new();
            for row in edge_stmt.query_map([&request_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)))? {
                let (id, source_kind, source_id, relation, target_kind, target_id) = row?;
                let source = node_id(&source_kind, &source_id);
                let target = node_id(&target_kind, &target_id);
                add(&source_kind, &source_id, format!("{source_kind} {source_id}"), "known".into(), true);
                add(&target_kind, &target_id, format!("{target_kind} {target_id}"), "known".into(), true);
                edges.push(json!({"id":id,"source":source,"target":target,"relation":relation}));
            }
            breaks.sort_by_key(|item| item.0);
            let earliest_id = breaks.first().map(|(_, kind, id, _)| node_id(kind, id));
            let mut neighborhood = HashMap::<String, Vec<String>>::new();
            for edge in &edges {
                let Some(source) = edge["source"].as_str() else { continue };
                let Some(target) = edge["target"].as_str() else { continue };
                neighborhood.entry(source.to_string()).or_default().push(target.to_string());
                neighborhood.entry(target.to_string()).or_default().push(source.to_string());
            }
            let seed = earliest_id.as_ref().filter(|id| node_ids.contains(*id)).cloned()
                .unwrap_or_else(|| node_id("request", &request));
            let mut selected = HashSet::<String>::new();
            let mut selected_order = Vec::<String>::new();
            let mut queue = VecDeque::from([seed]);
            while let Some(id) = queue.pop_front() {
                if selected_order.len() == 400 || !selected.insert(id.clone()) { continue; }
                selected_order.push(id.clone());
                if let Some(neighbors) = neighborhood.get(&id) { queue.extend(neighbors.iter().cloned()); }
            }
            for node in &nodes {
                if selected_order.len() == 400 { break; }
                let Some(id) = node["id"].as_str() else { continue };
                if selected.insert(id.to_string()) { selected_order.push(id.to_string()); }
            }
            let total_nodes = nodes.len();
            let total_edges = edges.len();
            let mut nodes_by_id = nodes.drain(..).filter_map(|node| {
                let id = node["id"].as_str()?.to_string();
                Some((id, node))
            }).collect::<HashMap<_, _>>();
            nodes = selected_order.iter().filter_map(|id| nodes_by_id.remove(id)).collect();
            let visible = nodes.iter().filter_map(|node| node["id"].as_str().map(str::to_string)).collect::<HashSet<_>>();
            edges.retain(|edge| {
                edge["source"].as_str().is_some_and(|id| visible.contains(id))
                    && edge["target"].as_str().is_some_and(|id| visible.contains(id))
            });
            edges.truncate(2000);
            for edge in &edges {
                let Some(source) = edge["source"].as_str() else { continue };
                let Some(target) = edge["target"].as_str() else { continue };
                linked.insert(source.to_string());
                linked.insert(target.to_string());
                downstream.entry(source.to_string()).or_default().push(target.to_string());
                upstream.entry(target.to_string()).or_default().push(source.to_string());
            }
            let mut unknown = Vec::new();
            for node in &mut nodes {
                let id = node["id"].as_str().unwrap_or_default().to_string();
                node["upstream"] = json!(upstream.get(&id).cloned().unwrap_or_default());
                node["downstream"] = json!(downstream.get(&id).cloned().unwrap_or_default());
                if node["kind"] != json!("request") && !linked.contains(&id) {
                    node["provenance"] = json!("unknown");
                    unknown.push(json!({"node_id":id,"reason":"no recorded provenance edge"}));
                }
            }
            let earliest = breaks.first().map(|(_, kind, id, reason)| json!({"node_id":node_id(kind,id),"kind":kind,"row_id":id,"reason":safety::redact(reason),"known":true})).unwrap_or_else(|| json!({"node_id":Value::Null,"kind":"unknown","row_id":Value::Null,"reason":"no recorded failure or recovery break was found","known":false}));
            unknown.truncate(400);
            let returned_nodes = nodes.len();
            let returned_edges = edges.len();
            let omitted_nodes = total_nodes.saturating_sub(returned_nodes);
            let omitted_edges = total_edges.saturating_sub(returned_edges);
            let node_cursor = (omitted_nodes > 0).then(|| json!({"projection":"causal-neighborhood-v1","break_node_id":earliest_id,"after_node_id":nodes.last().and_then(|node| node["id"].as_str())}));
            let edge_cursor = (omitted_edges > 0).then(|| json!({"projection":"causal-neighborhood-v1","break_node_id":earliest_id,"after_edge_id":edges.last().and_then(|edge| edge["id"].as_str())}));
            let truncation = json!({"nodes":omitted_nodes > 0,"edges":omitted_edges > 0});
            Ok(Some(json!({"request":{"request_id":request,"session_id":session,"scope":scope,"state":state,"updated_at":updated_at},"nodes":nodes,"edges":edges,"unknown_provenance":unknown,"earliest_known_break":earliest,"bounds":{"max_nodes":400,"max_edges":2000},"truncated":truncation,"counts":{"nodes":{"total":total_nodes,"returned":returned_nodes,"omitted":omitted_nodes},"edges":{"total":total_edges,"returned":returned_edges,"omitted":omitted_edges}},"expansion_cursors":{"nodes":node_cursor,"edges":edge_cursor}})))
        }).await
    }

    /// One recorded change by id, with the scope and session of the turn that made it. P2-T03's
    /// revert resolves the project root from this row and never from the caller, so a revert
    /// cannot be pointed at another project's files.
    pub async fn file_change(&self, id: String) -> Result<Option<Value>> {
        self.run(move|c|{
            Ok(c.query_row(crate::agentic_sql::FILE_CHANGE_GET,[id],|r|Ok(json!({
                "id":r.get::<_,String>(0)?,"request_id":r.get::<_,String>(1)?,"step_id":r.get::<_,String>(2)?,
                "path":r.get::<_,String>(3)?,"action":r.get::<_,String>(4)?,"before_hash":r.get::<_,Option<String>>(5)?,
                "after_hash":r.get::<_,Option<String>>(6)?,"diff":r.get::<_,Option<String>>(7)?,
                "applied":r.get::<_,i64>(8)?==1,"reverted_at":r.get::<_,Option<String>>(9)?,
                "scope":r.get::<_,String>(10)?,"session_id":r.get::<_,String>(11)?,
            }))).optional()?)
        }).await
    }
    /// Record an undo that already happened on disk, together with the activity event that
    /// announces it, in one transaction. `false` means the row was already reverted and nothing
    /// was written twice — which is what a double-clicked Revert has to look like.
    pub async fn record_revert(
        &self,
        id: String,
        request: String,
        session: String,
        step: String,
        path: String,
    ) -> Result<bool> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            if tx.execute(crate::agentic_sql::FILE_CHANGE_REVERTED, params![id, stamp])? != 1 {
                return Ok(false);
            }
            tx.execute(
                crate::agentic_sql::EVENT,
                params![
                    request,
                    session,
                    step,
                    "file_reverted",
                    json!({"change_id":id,"path":path}).to_string(),
                    stamp
                ],
            )?;
            tx.commit()?;
            Ok(true)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// One finished turn ('complete') and one live turn ('generating'), both with old rows, so a
    /// maintenance test can prove live turns and terminal evidence are never touched.
    async fn seed_retention_fixture(db: &DbStore) {
        db.run(|c| {
            let old = "2020-01-01T00:00:00+00:00";
            c.execute(
                "INSERT INTO sessions(id,scope,created_at) VALUES('s1','proj',?1)",
                [old],
            )?;
            for (request_id, state) in [("done-1", "complete"), ("live-1", "generating")] {
                c.execute(
                    "INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?1,'s1','user','hi','complete',?2)",
                    params![request_id, old],
                )?;
                let answer_id = if state == "complete" {
                    let answer_id = format!("{request_id}-answer");
                    c.execute(
                        "INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?1,'s1','assistant','hello','complete',?2)",
                        params![answer_id, old],
                    )?;
                    Some(answer_id)
                } else {
                    None
                };
                c.execute(
                    "INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,answer_id,captured_at,updated_at) VALUES(?1,'s1','proj','m',?2,0,?3,?4,?5,?5)",
                    params![request_id, format!("sig-{request_id}"), state, answer_id, old],
                )?;
                for piece in ["he", "ll", "o"] {
                    c.execute(
                        "INSERT INTO generation_events(request_id,session_id,state,content,created_at) VALUES(?1,'s1','chunk',?2,?3)",
                        params![request_id, piece, old],
                    )?;
                }
                c.execute(
                    "INSERT INTO activity_events(request_id,session_id,kind,payload_json,created_at) VALUES(?1,'s1','tool_started','{}',?2)",
                    params![request_id, old],
                )?;
            }
            c.execute(
                "INSERT INTO generation_events(request_id,session_id,state,content,created_at) VALUES('done-1','s1','completed','',?1)",
                [old],
            )?;
            Ok(json!({}))
        })
        .await
        .unwrap();
    }

    async fn count(db: &DbStore, sql: &'static str) -> i64 {
        db.read(move |c| Ok(c.query_row(sql, [], |r| r.get::<_, i64>(0))?))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn retention_policies_start_disabled_and_reject_unknown_targets() {
        let db = DbStore::init(":memory:").unwrap();
        seed_retention_fixture(&db).await;
        let policies = db.retention_policies().await.unwrap();
        assert_eq!(policies["policies"].as_array().unwrap().len(), 2);
        assert!(policies["policies"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["enabled"] == false));
        assert!(db
            .set_retention_policy("chat_receipts".into(), 30, true)
            .await
            .is_err());
        assert!(db
            .set_retention_policy("activity_events".into(), 0, true)
            .await
            .is_err());
        // A disabled policy must delete nothing, even with expired rows present.
        let report = db.apply_retention().await.unwrap();
        assert_eq!(report["activity_events"]["status"], "disabled");
        assert_eq!(count(&db, "SELECT count(*) FROM activity_events").await, 2);
    }

    #[tokio::test]
    async fn retention_and_compaction_preserve_receipts_and_live_turns() {
        let db = DbStore::init(":memory:").unwrap();
        seed_retention_fixture(&db).await;
        let compaction = db.compact_generation_chunks(1).await.unwrap();
        assert_eq!(compaction["requests_compacted"], 1);
        let merged: (String, i64) = db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT content,compacted_chunks FROM generation_events WHERE request_id='done-1' AND state='chunk'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(merged, ("hello".to_string(), 3));
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM generation_events WHERE request_id='live-1' AND state='chunk'"
            )
            .await,
            3,
            "a live turn must never be compacted"
        );

        db.set_retention_policy("generation_chunks".into(), 1, true)
            .await
            .unwrap();
        db.set_retention_policy("activity_events".into(), 1, true)
            .await
            .unwrap();
        let report = db.apply_retention().await.unwrap();
        assert_eq!(report["generation_chunks"]["status"], "applied");
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM generation_events WHERE request_id='done-1' AND state='completed'"
            )
            .await,
            1,
            "terminal generation rows must survive retention"
        );
        assert_eq!(
            count(&db, "SELECT count(*) FROM chat_receipts").await,
            2,
            "receipts are never deleted by maintenance"
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM activity_events WHERE request_id='live-1'"
            )
            .await,
            1,
            "a live turn must never be trimmed"
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM maintenance_runs WHERE action='retention'"
            )
            .await,
            2
        );
    }

    #[tokio::test]
    async fn retention_maintenance_records_wal_checkpoint_evidence() {
        let db = DbStore::init(":memory:").unwrap();
        seed_retention_fixture(&db).await;
        let result = db.maintenance().await.unwrap();
        assert!(result["incremental_vacuum"].is_string());
        assert!(result["freelist_after"].is_i64());
        let readiness = db.readiness().await.unwrap();
        assert_eq!(readiness["schema_version"], 10);
        assert_eq!(readiness["ready"], true);
        assert!(readiness["maintenance"]["last_wal_checkpoint_at"].is_string());
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM maintenance_runs WHERE action='wal_checkpoint'"
            )
            .await,
            1
        );
    }
    #[tokio::test]
    async fn import_is_idempotent_and_not_active() {
        let db = DbStore::init(":memory:").unwrap();
        let events = vec![Event {
            id: "event-1:part-0".into(),
            role: "user".into(),
            content: "I prefer Rust".into(),
        }];
        let a = db
            .ingest(
                "global".into(),
                "test".into(),
                "claude".into(),
                "safe source".into(),
                "digest".into(),
                vec![],
                vec![events.clone()],
            )
            .await
            .unwrap();
        assert_eq!(a["duplicate"], false);
        let b = db
            .ingest(
                "global".into(),
                "test".into(),
                "claude".into(),
                "safe source".into(),
                "digest".into(),
                vec![],
                vec![events],
            )
            .await
            .unwrap();
        assert_eq!(b["duplicate"], true);
        let job = db.claim_job().await.unwrap().unwrap();
        db.finish_job(
            job,
            vec![Proposal {
                key: "language".into(),
                value: "Rust".into(),
                category: "preference".into(),
                evidence_id: "event-1:part-0".into(),
                quote: "I prefer Rust".into(),
                priority: None,
            }],
        )
        .await
        .unwrap();
        assert_eq!(db.stats().await.unwrap()["active_memories"], 0);
        let list = db.candidates("global".into()).await.unwrap();
        let id = list["candidates"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(
            db.resolve(id.clone(), "wrong-scope".into(), true)
                .await
                .unwrap(),
            "not_found"
        );
        assert_eq!(
            db.resolve(id.clone(), "global".into(), true).await.unwrap(),
            "approved"
        );
        assert_eq!(
            db.resolve(id, "global".into(), false).await.unwrap(),
            "already_resolved"
        );
        assert_eq!(
            db.recall("global".into(), "Rust:".into())
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn hybrid_recall_uses_offline_vectors_shadowing_usefulness_and_budget() {
        let db = DbStore::init(":memory:").unwrap();
        db.run(|c|{let stamp=now();let rows=[
            ("c-rust","m-rust","global","language","Rust systems"),("c-global-db","m-global-db","global","database","Postgres"),
            ("c-project-db","m-project-db","proj","database","SQLite"),("c-fruit","m-fruit","global","fruit","Banana orchestra")];
            for (candidate,memory,scope,key,value) in rows {c.execute("INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at,resolved_at) VALUES(?1,?2,?3,?4,'fact',?5,'{}',0,'approved',?6,2000000000,?6)",params![candidate,scope,key,value,format!("source-{candidate}"),stamp])?;c.execute("INSERT INTO memories(id,scope,key,value,category,status,revision,candidate_id,created_at,updated_at) VALUES(?1,?2,?3,?4,'fact','active',1,?5,?6,?6)",params![memory,scope,key,value,candidate,stamp])?;}Ok(())}).await.unwrap();
        let first = db
            .recall("proj".into(), "rustacean database".into())
            .await
            .unwrap();
        assert!(
            first.iter().any(|memory| memory.id == "m-rust"),
            "vector recall should bridge related spelling"
        );
        assert!(first.iter().any(|memory| memory.id == "m-project-db"));
        assert!(
            !first.iter().any(|memory| memory.id == "m-global-db"),
            "project key shadows global key"
        );
        assert!(
            first
                .iter()
                .map(|memory| serde_json::to_vec(memory).unwrap().len())
                .sum::<usize>()
                <= 6000
        );
        db.run(|c| {
            let count: i64 =
                c.query_row("SELECT count(*) FROM memory_embeddings", [], |r| r.get(0))?;
            assert_eq!(count, 3);
            let bytes: i64 = c.query_row(
                "SELECT length(vector) FROM memory_embeddings WHERE memory_id='m-rust'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(bytes, (crate::embeddings::DIMENSIONS * 4) as i64);
            c.execute(
                "UPDATE memory_embeddings SET useful_count=20 WHERE memory_id='m-rust'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let reranked = db
            .recall("proj".into(), "database rustacean".into())
            .await
            .unwrap();
        assert_eq!(
            reranked.first().map(|memory| memory.id.as_str()),
            Some("m-rust"),
            "prior usefulness participates in reranking"
        );
    }

    #[tokio::test]
    async fn extraction_corrections_are_high_priority_and_candidate_feeds_are_scoped() {
        let db = DbStore::init(":memory:").unwrap();
        let correction = "No, use SQLite instead.";
        db.ingest(
            "global".into(),
            "turn".into(),
            "jsonl".into(),
            correction.into(),
            "correction-source".into(),
            vec![],
            vec![vec![Event {
                id: "event-correction".into(),
                role: "user".into(),
                content: correction.into(),
            }]],
        )
        .await
        .unwrap();
        let job = db.claim_job().await.unwrap().unwrap();
        db.finish_job(
            job,
            vec![Proposal {
                key: "database_choice".into(),
                value: "SQLite".into(),
                category: "decision".into(),
                evidence_id: "event-correction".into(),
                quote: correction.into(),
                priority: None,
            }],
        )
        .await
        .unwrap();
        let imports = db
            .candidate_feed("global".into(), None, true, false)
            .await
            .unwrap();
        let candidate = &imports["candidates"][0];
        assert_eq!(
            (
                candidate["priority"].as_str(),
                candidate["evidence"]["correction"].as_bool()
            ),
            (Some("high"), Some(true))
        );
        let id = candidate["id"].as_str().unwrap().to_string();
        assert_eq!(
            db.edit_candidate(id, "global".into(), "SQLite with WAL".into())
                .await
                .unwrap(),
            "edited"
        );
        let edited = db
            .candidate_feed("global".into(), None, true, false)
            .await
            .unwrap();
        assert_eq!(edited["candidates"][0]["value"], "SQLite with WAL");
        assert_eq!(edited["candidates"][0]["evidence"]["edited"], true);
        let request = uid();
        let chat_candidate = uid();
        db.run({let request=request.clone();let chat_candidate=chat_candidate.clone();move|c|{c.execute("INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at) VALUES(?1,'global','chat_choice','Rust','decision',?2,'{}',0,'pending',?3,2000000000)",params![chat_candidate,format!("chat:{request}"),now()])?;Ok(())}}).await.unwrap();
        let chat = db
            .candidate_feed("global".into(), Some(request.clone()), false, true)
            .await
            .unwrap();
        assert_eq!(chat["candidates"][0]["request_id"], request);
        assert_eq!(
            db.candidate_feed("global".into(), None, true, false)
                .await
                .unwrap()["candidates"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "chat candidates stay out of the import inbox"
        );
    }

    fn scope_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("harness-scope-{}", uid()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    #[tokio::test]
    async fn verification_model_settings_are_allowed_but_unknown_roles_are_not() {
        let db = DbStore::init(":memory:").unwrap();
        let mut settings = std::collections::BTreeMap::new();
        settings.insert("verification".into(), "cheap-verifier".into());
        db.set_settings(settings).await.unwrap();
        assert_eq!(
            db.settings().await.unwrap()["verification"],
            "cheap-verifier"
        );
        assert_eq!(
            db.role_model("verification", "main-model").await.unwrap(),
            "cheap-verifier"
        );
        let mut unknown = std::collections::BTreeMap::new();
        unknown.insert("judge".into(), "model".into());
        assert!(db.set_settings(unknown).await.is_err());
    }

    #[tokio::test]
    async fn provenance_edges_are_typed_scoped_and_row_backed() {
        let db = DbStore::init(":memory:").unwrap();
        db.run(|c| {
            let stamp = now();
            c.execute("INSERT INTO sessions(id,scope,created_at) VALUES('session','global',?1)",[&stamp])?;
            c.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES('request','session','user','hi','pending',?1)",[&stamp])?;
            c.execute("INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES('request','session','global','main','sig',0,'generating',?1,?1)",[&stamp])?;
            c.execute(crate::agentic_sql::STEP_BEGIN,params!["step","request",None::<String>,0,"tool_call","edit","call","{}",stamp])?;
            c.execute(crate::agentic_sql::PERMISSION_CREATE,params!["permit","request","step","edit","edit","{}",now(),now()])?;
            Ok(())
        }).await.unwrap();
        let id = db
            .record_provenance_edge(
                "request".into(),
                "step".into(),
                "step".into(),
                "depends_on".into(),
                "permission".into(),
                "permit".into(),
            )
            .await
            .unwrap();
        let edges = db.provenance_edges("request".into()).await.unwrap();
        assert_eq!(
            (
                edges["edges"][0]["id"].as_str(),
                edges["edges"][0]["relation"].as_str()
            ),
            (Some(id.as_str()), Some("depends_on"))
        );
        assert!(db
            .record_provenance_edge(
                "request".into(),
                "claim".into(),
                "x".into(),
                "supports".into(),
                "step".into(),
                "step".into(),
            )
            .await
            .is_err());
        assert!(db
            .record_provenance_edge(
                "request".into(),
                "step".into(),
                "missing".into(),
                "supports".into(),
                "permission".into(),
                "permit".into(),
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn bounded_incident_graph_never_returns_dangling_references() {
        let db = DbStore::init(":memory:").unwrap();
        db.run(|c| {
            let stamp = now();
            c.execute("INSERT INTO sessions(id,scope,created_at) VALUES('session','global',?1)",[&stamp])?;
            c.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES('request','session','user','hi','pending',?1)",[&stamp])?;
            c.execute("INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES('request','session','global','main','sig',0,'generating',?1,?1)",[&stamp])?;
            for seq in 0..200 {
                let step = format!("step-{seq:03}");
                let permit = format!("permit-{seq:03}");
                c.execute(crate::agentic_sql::STEP_BEGIN,params![step,"request",None::<String>,seq,"tool_call","edit",format!("call-{seq}"),"{}",stamp])?;
                c.execute(crate::agentic_sql::PERMISSION_CREATE,params![permit,"request",step,"edit","edit","{}",stamp,stamp])?;
                c.execute(crate::agentic_sql::PROVENANCE_EDGE_INSERT,params![format!("edge-{seq:03}"),"request","step",step,"depends_on","permission",permit,stamp])?;
            }
            c.execute(crate::agentic_sql::STEP_FINISH,params!["step-199","failed","{}",0,0,None::<i64>,None::<i64>,"late_failure",stamp])?;
            Ok(())
        }).await.unwrap();

        let graph = db.incident_graph("request".into()).await.unwrap().unwrap();
        let nodes = graph["nodes"].as_array().unwrap();
        let node_ids = nodes
            .iter()
            .map(|node| node["id"].as_str().unwrap())
            .collect::<HashSet<_>>();
        assert_eq!(nodes.len(), 400);
        assert_eq!(graph["truncated"], json!({"nodes":true,"edges":true}));
        assert_eq!(
            graph["counts"]["nodes"],
            json!({"total":401,"returned":400,"omitted":1})
        );
        assert_eq!(
            graph["counts"]["edges"],
            json!({"total":200,"returned":199,"omitted":1})
        );
        assert_eq!(graph["earliest_known_break"]["node_id"], "step:step-199");
        assert!(node_ids.contains("step:step-199"));
        assert!(node_ids.contains(
            graph["expansion_cursors"]["nodes"]["after_node_id"]
                .as_str()
                .unwrap()
        ));
        assert!(graph["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edge| { edge["id"] == graph["expansion_cursors"]["edges"]["after_edge_id"] }));
        for edge in graph["edges"].as_array().unwrap() {
            assert!(node_ids.contains(edge["source"].as_str().unwrap()));
            assert!(node_ids.contains(edge["target"].as_str().unwrap()));
        }
        for node in nodes {
            for direction in ["upstream", "downstream"] {
                for endpoint in node[direction].as_array().unwrap() {
                    assert!(node_ids.contains(endpoint.as_str().unwrap()));
                }
            }
        }
    }

    #[tokio::test]
    async fn turn_steps_projects_a_bounded_latest_verification() {
        let db = DbStore::init(":memory:").unwrap();
        let claims=(0..25).map(|index|json!({"claim":if index==0{"c".repeat(600)}else{format!("claim {index}")},
            "status":"unverified","evidence_step_ids":(0..10).map(|id|format!("step-{id}")).collect::<Vec<_>>(),
            "reason":if index==0{"r".repeat(600)}else{"missing evidence".into()}})).collect::<Vec<_>>();
        let diagnostics = (0..15)
            .map(|index| format!("diagnostic {index}"))
            .collect::<Vec<_>>();
        let output=json!({"status":"unverified","claims":claims,"skipped_diagnostics":diagnostics,"model":"cheap-verifier"}).to_string();
        db.run(move|c|{
            let stamp=now();
            c.execute("INSERT INTO sessions(id,scope,created_at) VALUES('session','global',?1)",[&stamp])?;
            c.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES('request','session','user','hi','pending',?1)",[&stamp])?;
            c.execute("INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES('request','session','global','main','sig',0,'generating',?1,?1)",[&stamp])?;
            c.execute(crate::agentic_sql::STEP_BEGIN,params!["verify-step","request",None::<String>,0,"verification",None::<String>,None::<String>,"{}",stamp])?;
            c.execute(crate::agentic_sql::STEP_FINISH,params!["verify-step","complete",output,0,0,None::<i64>,None::<i64>,None::<String>,now()])?;
            Ok(())
        }).await.unwrap();
        let response = db.turn_steps("request".into()).await.unwrap();
        let verification = &response["verification"];
        assert_eq!(
            (
                verification["status"].as_str(),
                verification["unverified_claims"].as_u64()
            ),
            (Some("unverified"), Some(20))
        );
        assert_eq!(
            verification["claims"].as_array().unwrap().len(),
            MAX_VERIFICATION_PROJECTION_CLAIMS
        );
        assert_eq!(
            verification["claims"][0]["claim"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            500
        );
        assert_eq!(
            verification["claims"][0]["reason"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            500
        );
        assert_eq!(
            verification["claims"][0]["evidence_step_ids"]
                .as_array()
                .unwrap()
                .len(),
            MAX_VERIFICATION_PROJECTION_EVIDENCE_IDS
        );
        assert_eq!(
            verification["skipped_diagnostics"]
                .as_array()
                .unwrap()
                .len(),
            MAX_VERIFICATION_PROJECTION_DIAGNOSTICS
        );
    }

    #[tokio::test]
    async fn scopes_merge_partial_updates_and_gate_tools() {
        let db = DbStore::init(":memory:").unwrap();
        assert!(
            db.scope_config("global".into()).await.unwrap().is_none(),
            "an unconfigured scope must read as absent, not as defaults"
        );
        let dir = scope_dir();
        let canonical = std::fs::canonicalize(&dir).unwrap();
        let patch = ScopePatch {
            root_path: Some(Some(dir.to_string_lossy().into())),
            permission_mode: Some("auto_edit".into()),
            diagnostics_cmd: Some(Some("  cargo check -q  ".into())),
            max_steps: Some(Some(12)),
            ..Default::default()
        }
        .validate()
        .unwrap();
        let saved = db.upsert_scope("global".into(), patch).await.unwrap();
        assert_eq!(
            saved.root_path.as_deref(),
            canonical.to_str(),
            "root_path is stored canonicalized"
        );
        assert_eq!(saved.diagnostics_cmd.as_deref(), Some("cargo check -q"));
        assert_eq!(saved.mode(), crate::tools::PermissionMode::AutoEdit);
        assert_eq!(
            saved.budgets(),
            (12, DEFAULT_MAX_TOOL_BYTES, DEFAULT_MAX_WALL_SECONDS)
        );
        assert!(saved.tool_ctx("request", "step").is_some());
        let touched = db
            .upsert_scope(
                "global".into(),
                ScopePatch {
                    permission_mode: Some("ask".into()),
                    ..Default::default()
                }
                .validate()
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            (
                touched.root_path,
                touched.diagnostics_cmd,
                touched.max_steps,
                touched.created_at
            ),
            (
                saved.root_path,
                saved.diagnostics_cmd,
                saved.max_steps,
                saved.created_at
            )
        );
        let cleared = db
            .upsert_scope(
                "global".into(),
                ScopePatch {
                    root_path: Some(None),
                    diagnostics_cmd: Some(None),
                    ..Default::default()
                }
                .validate()
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            cleared.root_path.is_none() && cleared.diagnostics_cmd.is_none(),
            "an explicit null clears the column"
        );
        assert!(
            cleared.tool_ctx("request", "step").is_none(),
            "a scope without root_path must not hand a working directory to any tool"
        );
        std::fs::remove_dir_all(dir).ok();
    }
    /// An empty patch carries no intent. Rewriting the row anyway moved `updated_at`, which is
    /// how a settings-shaped probe could look like a change that never happened.
    #[tokio::test]
    async fn an_empty_patch_leaves_the_stored_row_untouched() {
        let db = DbStore::init(":memory:").unwrap();
        let saved = db
            .upsert_scope(
                "global".into(),
                ScopePatch {
                    permission_mode: Some("auto_edit".into()),
                    max_steps: Some(Some(7)),
                    ..Default::default()
                }
                .validate()
                .unwrap(),
            )
            .await
            .unwrap();
        let untouched = db
            .upsert_scope("global".into(), ScopePatch::default().validate().unwrap())
            .await
            .unwrap();
        assert_eq!(
            (
                &untouched.permission_mode,
                &untouched.max_steps,
                &untouched.created_at,
                &untouched.updated_at
            ),
            (
                &saved.permission_mode,
                &saved.max_steps,
                &saved.created_at,
                &saved.updated_at
            ),
            "an empty patch must not rewrite the row, not even its updated_at"
        );
        let created = db
            .upsert_scope("fresh".into(), ScopePatch::default().validate().unwrap())
            .await
            .unwrap();
        assert_eq!(created.permission_mode, "ask");
        assert!(
            db.scope_config("fresh".into()).await.unwrap().is_some(),
            "posting to a scope that does not exist still creates it"
        );
    }
    #[test]
    fn scopes_reject_unusable_configuration() {
        let dir = scope_dir();
        let file = dir.join("Cargo.toml");
        std::fs::write(&file, "x").unwrap();
        let rejected = [
            ScopePatch {
                root_path: Some(Some("relative/dir".into())),
                ..Default::default()
            },
            ScopePatch {
                root_path: Some(Some(dir.join("missing").to_string_lossy().into())),
                ..Default::default()
            },
            ScopePatch {
                root_path: Some(Some(file.to_string_lossy().into())),
                ..Default::default()
            },
            ScopePatch {
                permission_mode: Some("root".into()),
                ..Default::default()
            },
            ScopePatch {
                diagnostics_cmd: Some(Some("x".repeat(513))),
                ..Default::default()
            },
            ScopePatch {
                diagnostics_cmd: Some(Some("cargo check; rm -rf /".into())),
                ..Default::default()
            },
            ScopePatch {
                max_steps: Some(Some(0)),
                ..Default::default()
            },
            ScopePatch {
                max_tool_bytes: Some(Some(64)),
                ..Default::default()
            },
            ScopePatch {
                max_wall_seconds: Some(Some(5)),
                ..Default::default()
            },
        ];
        for bad in rejected {
            assert!(
                bad.clone().validate().is_err(),
                "expected a rejection for {bad:?}"
            );
        }
        assert!(
            canonical_root("/").is_err(),
            "the filesystem root is never a project root"
        );
        std::fs::remove_dir_all(dir).ok();
    }
    #[tokio::test]
    async fn plans_are_replaced_whole_or_not_at_all() {
        let db = DbStore::init(":memory:").unwrap();
        db.run(|c| {
            c.execute("INSERT INTO sessions VALUES('s','global','now')", [])?;
            Ok(())
        })
        .await
        .unwrap();
        assert!(
            db.plan("s".into()).await.unwrap()["items"]
                .as_array()
                .unwrap()
                .is_empty(),
            "a session starts without a plan"
        );
        let first = replace(
            &db,
            vec![
                ("  read the failing test  ".into(), "done".into()),
                ("fix the anchor".into(), "in_progress".into()),
            ],
        )
        .await
        .unwrap();
        assert_eq!(first["items"][0]["text"], "read the failing test");
        assert_eq!(
            (&first["items"][1]["seq"], &first["items"][1]["status"]),
            (&json!(2), &json!("in_progress"))
        );
        let second = replace(&db, vec![("ship it".into(), "pending".into())])
            .await
            .unwrap();
        assert_eq!(
            second["items"].as_array().unwrap().len(),
            1,
            "a replacement plan must not merge with the previous one"
        );
        for bad in [
            vec![
                ("a".into(), "in_progress".into()),
                ("b".into(), "in_progress".into()),
            ],
            vec![("  ".into(), "pending".into())],
            vec![("x".repeat(MAX_PLAN_TEXT + 1), "pending".into())],
            vec![("x".into(), "blocked".into())],
            (0..=MAX_PLAN_ITEMS)
                .map(|i| (format!("step {i}"), "pending".to_string()))
                .collect::<Vec<_>>(),
        ] {
            assert!(replace(&db, bad).await.is_err());
        }
        assert_eq!(
            db.plan("s".into()).await.unwrap()["items"],
            second["items"],
            "a refused plan leaves the stored one untouched"
        );
    }

    /// `write_plan` always runs inside the caller's transaction — for the agent loop that is the
    /// transaction finishing the step that produced the plan. This mirrors that shape without
    /// pulling the loop into a storage test.
    async fn replace(db: &DbStore, items: Vec<(String, String)>) -> Result<Value> {
        db.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stored = write_plan(&tx, "s", &items)?;
            tx.commit()?;
            Ok(stored)
        })
        .await
    }

    fn spend_limits() -> SpendLimits {
        SpendLimits {
            requests_per_turn: 2,
            requests_per_day: 20,
            tokens_per_turn: Some(100),
            tokens_per_day: Some(1000),
            cost_microusd_per_turn: Some(100),
            cost_microusd_per_day: Some(1000),
            input_microusd_per_million: Some(1_000_000),
            output_microusd_per_million: Some(1_000_000),
        }
    }

    #[tokio::test]
    async fn provider_spend_reservations_fail_closed_and_persist_reasons() {
        let db = DbStore::init(":memory:").unwrap();
        let limits = spend_limits();
        let first = db
            .reserve_provider_call(
                Some("request".into()),
                "model_call".into(),
                "model".into(),
                limits.clone(),
            )
            .await
            .unwrap();
        db.finish_provider_call(
            first,
            ModelUsage {
                prompt_tokens: Some(60),
                completion_tokens: Some(40),
                unavailable_reason: None,
            },
            limits.clone(),
            None,
        )
        .await
        .unwrap();
        let error = db
            .reserve_provider_call(
                Some("request".into()),
                "model_call".into(),
                "model".into(),
                limits.clone(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("turn_token_limit"));
        let refused:i64=db.run(|c|Ok(c.query_row("SELECT count(*) FROM provider_calls WHERE state='refused' AND reason='turn_token_limit'",[],|r|r.get(0))?)).await.unwrap();
        assert_eq!(refused, 1);
    }

    #[tokio::test]
    async fn unknown_usage_and_missing_pricing_are_not_counted_as_zero() {
        let db = DbStore::init(":memory:").unwrap();
        let limits = spend_limits();
        let first = db
            .reserve_provider_call(
                Some("unknown".into()),
                "model_call".into(),
                "model".into(),
                limits.clone(),
            )
            .await
            .unwrap();
        db.finish_provider_call(first, ModelUsage::default(), limits.clone(), None)
            .await
            .unwrap();
        let error = db
            .reserve_provider_call(
                Some("unknown".into()),
                "model_call".into(),
                "model".into(),
                limits.clone(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("turn_usage_unavailable"));
        let db = DbStore::init(":memory:").unwrap();
        let mut no_price = limits;
        no_price.input_microusd_per_million = None;
        let error = db
            .reserve_provider_call(
                Some("price".into()),
                "model_call".into(),
                "model".into(),
                no_price,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cost_pricing_unavailable"));
    }
}
