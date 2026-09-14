//! What a scope is allowed to be before any of it reaches the database.
//!
//! Scope limits, the plan constraints that mirror the schema CHECKs, the
//! patch type that applies partial updates, and root-path canonicalisation
//! live here; `DbStore` stays in the parent and only persists values that
//! these types have already validated.
use super::*;

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

pub(super) fn scope_row(r: &rusqlite::Row) -> rusqlite::Result<ScopeConfig> {
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
pub(super) fn plan_rows(c: &Connection, session_id: &str) -> Result<Value> {
    let mut stmt = c.prepare(crate::agentic_sql::PLAN_LIST)?;
    let rows = stmt.query_map([session_id], |r| {
        Ok(json!({
            "seq": r.get::<_, i64>(0)?, "text": r.get::<_, String>(1)?,
            "status": r.get::<_, String>(2)?, "updated_at": r.get::<_, String>(3)?,
        }))
    })?;
    Ok(json!({ "items": rows.collect::<rusqlite::Result<Vec<_>>>()? }))
}

pub(super) fn bounded_projection_text(value: Option<&str>, max: usize) -> String {
    value.unwrap_or_default().chars().take(max).collect()
}

pub(super) fn verification_projection(
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
pub(super) fn bounded(
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
