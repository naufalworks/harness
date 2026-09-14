use crate::{ingest::Event, limits::verification as vlimits, patch::Patch};
use anyhow::{bail, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
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

// P12-T02: the provenance writer/reader and the bounded incident projection now live in
// `storage/provenance.rs`. The constants stay re-exported from `crate::storage` so no
// caller path changes with the file move.
mod config;
mod jobs;
mod memories;
mod provenance;
mod provider;
mod turns;
#[allow(unused_imports)]
pub use provenance::{PROVENANCE_NODE_KINDS, PROVENANCE_RELATIONS};
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
// Verification projection caps live in `crate::limits::verification`: the producer-side
// validator in `memory_agents` rejects reports against the same numbers, so a local copy
// here could silently truncate a report that validation already accepted.

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
        .take(vlimits::MAX_CLAIMS).filter_map(|claim|{
            let claim_text=claim.get("claim")?.as_str()?;
            let claim_status=claim.get("status")?.as_str()?;
            if !matches!(claim_status,"verified"|"unverified") {return None;}
            let evidence_step_ids=claim.get("evidence_step_ids").and_then(Value::as_array).into_iter().flatten()
                .filter_map(Value::as_str).take(vlimits::MAX_EVIDENCE_IDS_PER_CLAIM)
                .map(|id|bounded_projection_text(Some(id),vlimits::MAX_IDENTIFIER_CHARS)).collect::<Vec<_>>();
            Some(json!({"claim":bounded_projection_text(Some(claim_text),vlimits::MAX_CLAIM_CHARS),"status":claim_status,
                "evidence_step_ids":evidence_step_ids,
                "reason":bounded_projection_text(claim.get("reason").and_then(Value::as_str),vlimits::MAX_REASON_CHARS)}))
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
            .take(vlimits::MAX_SKIPPED_DIAGNOSTICS)
            .map(|item| bounded_projection_text(Some(item), vlimits::MAX_DIAGNOSTIC_CHARS))
            .collect::<Vec<_>>()
    };
    let unverified_claims = claims
        .iter()
        .filter(|claim| claim["status"] == "unverified")
        .count();
    json!({"step_id":id,"status":status,"step_status":step_status,"claims":claims,
        "unverified_claims":unverified_claims,"skipped_diagnostics":skipped_diagnostics,
        "model":bounded_projection_text(parsed.get("model").and_then(Value::as_str),vlimits::MAX_IDENTIFIER_CHARS),
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

/// Absent / `null` / value are three different requests on this route, so every nullable field
/// is a `Patch<T>` rather than a nested `Option`. See `crate::patch`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopePatch {
    #[serde(default)]
    pub root_path: Patch<String>,
    /// Not nullable: the column always holds a mode, so `null` here is simply "unchanged".
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub diagnostics_cmd: Patch<String>,
    #[serde(default)]
    pub max_steps: Patch<i64>,
    #[serde(default)]
    pub max_tool_bytes: Patch<i64>,
    #[serde(default)]
    pub max_wall_seconds: Patch<i64>,
}

impl ScopePatch {
    /// True when the caller named no field at all. An empty patch states no intent, so
    /// `upsert_scope` answers with the stored row instead of rewriting it, and `updated_at`
    /// moves only when a value actually moved.
    pub fn is_empty(&self) -> bool {
        self.root_path.is_unchanged()
            && self.permission_mode.is_none()
            && self.diagnostics_cmd.is_unchanged()
            && self.max_steps.is_unchanged()
            && self.max_tool_bytes.is_unchanged()
            && self.max_wall_seconds.is_unchanged()
    }
    /// Normalize and reject before anything reaches SQLite, so a bad request is a 400 and
    /// never a CHECK-constraint failure. Canonicalizing `root_path` touches the filesystem.
    pub fn validate(mut self) -> std::result::Result<Self, &'static str> {
        if let Patch::Set(raw) = &self.root_path {
            let canonical = canonical_root(raw)?;
            self.root_path = Patch::Set(canonical);
        }
        if let Some(mode) = self.permission_mode.as_deref() {
            if crate::tools::PermissionMode::parse(mode).is_none() {
                return Err("permission_mode must be ask, auto_edit or auto_all");
            }
        }
        if let Patch::Set(raw) = &self.diagnostics_cmd {
            let cmd = raw.trim().to_string();
            if cmd.is_empty() {
                // An all-whitespace command states "no diagnostics", same as `null`.
                self.diagnostics_cmd = Patch::Clear;
            } else if cmd.chars().count() > 512 || cmd.chars().any(char::is_control) {
                return Err("diagnostics_cmd must be 1-512 characters without control characters");
            } else if crate::tools::is_dangerous_command(&cmd) {
                return Err("diagnostics_cmd matches the destructive-command deny-list");
            } else {
                self.diagnostics_cmd = Patch::Set(cmd);
            }
        }
        bounded(
            &self.max_steps,
            1,
            500,
            "max_steps must be between 1 and 500",
        )?;
        bounded(
            &self.max_tool_bytes,
            1024,
            50_000_000,
            "max_tool_bytes must be between 1024 and 50000000",
        )?;
        bounded(
            &self.max_wall_seconds,
            10,
            86_400,
            "max_wall_seconds must be between 10 and 86400",
        )?;
        Ok(self)
    }
}

/// Only a supplied value is range-checked; clearing a limit restores the built-in default.
fn bounded(
    value: &Patch<i64>,
    low: i64,
    high: i64,
    message: &'static str,
) -> std::result::Result<(), &'static str> {
    match value.value() {
        Some(n) if !(low..=high).contains(n) => Err(message),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_agents::{ModelUsage, SpendLimits};
    use rusqlite::TransactionBehavior;
    use std::collections::HashSet;
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

    /// P11-T04's readiness projection is what an operator actually reads, and now that the
    /// duplicate Rust retention surface is retired, `scripts/maintenance.py` is the only writer
    /// of `maintenance_runs`. This proves the projection reports, per action, what that writer
    /// recorded -- so the retention story stays verifiable from the server even though the
    /// server no longer performs it.
    #[tokio::test]
    async fn retention_and_maintenance_runs_surface_in_the_readiness_projection() {
        let db = DbStore::init(":memory:").unwrap();
        seed_retention_fixture(&db).await;
        let readiness = db.readiness().await.unwrap();
        assert_eq!(readiness["schema_version"], 10);
        assert_eq!(readiness["ready"], true);
        assert!(
            readiness["maintenance"]["last_retention_at"].is_null(),
            "a database that was never maintained must not claim it was"
        );
        assert!(readiness["maintenance"]["last_wal_checkpoint_at"].is_null());

        // Exactly what scripts/maintenance.py writes when it finishes each action.
        db.run(|c| {
            for (action, target, finished) in [
                ("retention", "activity_events", "2026-01-01T00:00:00+00:00"),
                ("compaction", "generation_chunks", "2026-01-02T00:00:00+00:00"),
                ("wal_checkpoint", "database", "2026-01-03T00:00:00+00:00"),
            ] {
                c.execute(
                    "INSERT INTO maintenance_runs(action,target,rows_affected,detail,started_at,finished_at) VALUES(?1,?2,0,'scripts/maintenance.py',?3,?3)",
                    params![action, target, finished],
                )?;
            }
            Ok(json!({}))
        })
        .await
        .unwrap();

        let readiness = db.readiness().await.unwrap();
        let maintenance = &readiness["maintenance"];
        assert_eq!(
            maintenance["last_retention_at"],
            "2026-01-01T00:00:00+00:00"
        );
        assert_eq!(
            maintenance["last_compaction_at"],
            "2026-01-02T00:00:00+00:00"
        );
        assert_eq!(
            maintenance["last_wal_checkpoint_at"], "2026-01-03T00:00:00+00:00",
            "each action must project its own latest run, not the newest row of any action"
        );
        assert_eq!(
            count(&db, "SELECT count(*) FROM chat_receipts").await,
            2,
            "reading readiness must never touch evidence"
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
            vlimits::MAX_CLAIMS
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
            vlimits::MAX_EVIDENCE_IDS_PER_CLAIM
        );
        assert_eq!(
            verification["skipped_diagnostics"]
                .as_array()
                .unwrap()
                .len(),
            vlimits::MAX_SKIPPED_DIAGNOSTICS
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
            root_path: Patch::Set(dir.to_string_lossy().into()),
            permission_mode: Some("auto_edit".into()),
            diagnostics_cmd: Patch::Set("  cargo check -q  ".into()),
            max_steps: Patch::Set(12),
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
                    root_path: Patch::Clear,
                    diagnostics_cmd: Patch::Clear,
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
                    max_steps: Patch::Set(7),
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
    /// The three wire states are three different requests. An absent field must not be read as
    /// `null`, or a partial settings save would silently wipe fields the caller never mentioned.
    #[test]
    fn scope_patch_json_separates_absent_null_and_value() {
        let absent: ScopePatch = serde_json::from_str("{}").unwrap();
        assert!(absent.is_empty(), "an empty body states no intent");
        assert!(matches!(absent.root_path, Patch::Unchanged));
        assert!(matches!(absent.max_steps, Patch::Unchanged));

        let cleared: ScopePatch =
            serde_json::from_str(r#"{"root_path":null,"max_steps":null}"#).unwrap();
        assert!(!cleared.is_empty(), "an explicit null is a stated change");
        assert!(matches!(cleared.root_path, Patch::Clear));
        assert!(matches!(cleared.max_steps, Patch::Clear));

        let set: ScopePatch =
            serde_json::from_str(r#"{"root_path":"/tmp","max_steps":9}"#).unwrap();
        assert_eq!(set.root_path.value().map(String::as_str), Some("/tmp"));
        assert_eq!(set.max_steps.value().copied(), Some(9));

        // A cleared limit carries no value, so range validation has nothing to reject.
        assert!(cleared.clone().validate().is_ok());
    }
    #[test]
    fn scopes_reject_unusable_configuration() {
        let dir = scope_dir();
        let file = dir.join("Cargo.toml");
        std::fs::write(&file, "x").unwrap();
        let rejected = [
            ScopePatch {
                root_path: Patch::Set("relative/dir".into()),
                ..Default::default()
            },
            ScopePatch {
                root_path: Patch::Set(dir.join("missing").to_string_lossy().into()),
                ..Default::default()
            },
            ScopePatch {
                root_path: Patch::Set(file.to_string_lossy().into()),
                ..Default::default()
            },
            ScopePatch {
                permission_mode: Some("root".into()),
                ..Default::default()
            },
            ScopePatch {
                diagnostics_cmd: Patch::Set("x".repeat(513)),
                ..Default::default()
            },
            ScopePatch {
                diagnostics_cmd: Patch::Set("cargo check; rm -rf /".into()),
                ..Default::default()
            },
            ScopePatch {
                max_steps: Patch::Set(0),
                ..Default::default()
            },
            ScopePatch {
                max_tool_bytes: Patch::Set(64),
                ..Default::default()
            },
            ScopePatch {
                max_wall_seconds: Patch::Set(5),
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
