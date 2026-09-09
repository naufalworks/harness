use anyhow::{bail, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use uuid::Uuid;
use crate::{ingest::Event, safety};

pub fn now() -> String { Utc::now().to_rfc3339() }
pub fn uid() -> String { Uuid::new_v4().to_string() }
#[derive(Clone)]
pub struct DbStore { conn: Arc<Mutex<Connection>>, permits: Arc<Semaphore> }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proposal { pub key:String, pub value:String, pub category:String, pub evidence_id:String, pub quote:String }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recall { pub id:String, pub scope:String, pub key:String, pub value:String, pub revision:i64, pub evidence:Value }
pub struct Job { pub id:String, pub scope:String, pub source_id:String, pub events:Vec<Event>, pub attempts:i64 }

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
        Self { scope: scope.into(), root_path: None, permission_mode: "ask".into(), diagnostics_cmd: None, max_steps: None, max_tool_bytes: None, max_wall_seconds: None, created_at: String::new(), updated_at: String::new() }
    }
    /// An unreadable stored value degrades to the safest mode instead of failing the turn.
    pub fn mode(&self) -> crate::tools::PermissionMode {
        crate::tools::PermissionMode::parse(&self.permission_mode).unwrap_or(crate::tools::PermissionMode::Ask)
    }
    /// `(max_steps, max_tool_bytes, max_wall_seconds)` with the design defaults applied.
    pub fn budgets(&self) -> (i64, i64, i64) {
        (self.max_steps.unwrap_or(DEFAULT_MAX_STEPS), self.max_tool_bytes.unwrap_or(DEFAULT_MAX_TOOL_BYTES), self.max_wall_seconds.unwrap_or(DEFAULT_MAX_WALL_SECONDS))
    }
    /// `None` means "chat only". The loop hands it to `Registry::invoke`, which then refuses
    /// every call instead of guessing a working directory.
    pub fn tool_ctx(&self, request_id: &str, step_id: &str) -> Option<crate::tools::ToolCtx> {
        let root = self.root_path.as_ref()?;
        Some(crate::tools::ToolCtx { root: root.into(), scope: self.scope.clone(), request_id: request_id.into(), step_id: step_id.into(), diagnostics_cmd: self.diagnostics_cmd.clone() })
    }
}

fn scope_row(r: &rusqlite::Row) -> rusqlite::Result<ScopeConfig> {
    Ok(ScopeConfig { scope: r.get(0)?, root_path: r.get(1)?, permission_mode: r.get(2)?, diagnostics_cmd: r.get(3)?, max_steps: r.get(4)?, max_tool_bytes: r.get(5)?, max_wall_seconds: r.get(6)?, created_at: r.get(7)?, updated_at: r.get(8)? })
}

/// An absent field leaves the stored column alone; an explicit `null` clears it.
/// The session plan in `seq` order. Shared by `plan` and `write_plan` so the value the tool
/// returns to the model is read back from the same rows the UI will show.
fn plan_rows(c: &Connection, session_id: &str) -> Result<Value> {
    let mut stmt = c.prepare(crate::agentic_sql::PLAN_LIST)?;
    let rows = stmt.query_map([session_id], |r| Ok(json!({
        "seq": r.get::<_, i64>(0)?, "text": r.get::<_, String>(1)?,
        "status": r.get::<_, String>(2)?, "updated_at": r.get::<_, String>(3)?,
    })))?;
    Ok(json!({ "items": rows.collect::<rusqlite::Result<Vec<_>>>()? }))
}

/// The plan limits from docs/design/tools.md#todo_write, checked before SQLite so a CHECK
/// failure never reaches the caller as an opaque database error.
pub(crate) fn validate_plan(items: &[(String, String)]) -> Result<()> {
    if items.len() > MAX_PLAN_ITEMS { bail!("a plan holds at most {MAX_PLAN_ITEMS} items"); }
    if items.iter().any(|(text, _)| text.trim().is_empty() || text.trim().chars().count() > MAX_PLAN_TEXT) { bail!("every plan item needs 1..{MAX_PLAN_TEXT} characters of text"); }
    if items.iter().any(|(_, status)| !PLAN_STATUSES.contains(&status.as_str())) { bail!("unknown plan item status"); }
    if items.iter().filter(|(_, status)| status == "in_progress").count() > 1 { bail!("only one plan item may be in_progress"); }
    Ok(())
}

/// Clear-and-insert inside the caller's transaction, returning the stored plan. The agent loop
/// calls this while finishing the step that produced the plan, so a plan and the step that
/// produced it become visible together or not at all.
pub(crate) fn write_plan(c: &Connection, session_id: &str, items: &[(String, String)]) -> Result<Value> {
    validate_plan(items)?;
    c.execute(crate::agentic_sql::PLAN_CLEAR, [session_id])?;
    let stamp = now();
    for (i, (text, status)) in items.iter().enumerate() {
        c.execute(crate::agentic_sql::PLAN_INSERT, params![uid(), session_id, i as i64 + 1, text.trim(), status, stamp])?;
    }
    plan_rows(c, session_id)
}

fn patch_field<'de, D, T>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where D: serde::Deserializer<'de>, T: Deserialize<'de> {
    Option::deserialize(d).map(Some)
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopePatch {
    #[serde(default, deserialize_with = "patch_field")] pub root_path: Option<Option<String>>,
    #[serde(default)] pub permission_mode: Option<String>,
    #[serde(default, deserialize_with = "patch_field")] pub diagnostics_cmd: Option<Option<String>>,
    #[serde(default, deserialize_with = "patch_field")] pub max_steps: Option<Option<i64>>,
    #[serde(default, deserialize_with = "patch_field")] pub max_tool_bytes: Option<Option<i64>>,
    #[serde(default, deserialize_with = "patch_field")] pub max_wall_seconds: Option<Option<i64>>,
}

impl ScopePatch {
    /// Normalize and reject before anything reaches SQLite, so a bad request is a 400 and
    /// never a CHECK-constraint failure. Canonicalizing `root_path` touches the filesystem.
    pub fn validate(mut self) -> std::result::Result<Self, &'static str> {
        if let Some(Some(raw)) = &self.root_path {
            let canonical = canonical_root(raw)?;
            self.root_path = Some(Some(canonical));
        }
        if let Some(mode) = self.permission_mode.as_deref() {
            if crate::tools::PermissionMode::parse(mode).is_none() { return Err("permission_mode must be ask, auto_edit or auto_all"); }
        }
        if let Some(Some(raw)) = &self.diagnostics_cmd {
            let cmd = raw.trim().to_string();
            if cmd.is_empty() { self.diagnostics_cmd = Some(None); }
            else if cmd.chars().count() > 512 || cmd.chars().any(char::is_control) { return Err("diagnostics_cmd must be 1-512 characters without control characters"); }
            else if crate::tools::is_dangerous_command(&cmd) { return Err("diagnostics_cmd matches the destructive-command deny-list"); }
            else { self.diagnostics_cmd = Some(Some(cmd)); }
        }
        bounded(self.max_steps, 1, 500, "max_steps must be between 1 and 500")?;
        bounded(self.max_tool_bytes, 1024, 50_000_000, "max_tool_bytes must be between 1024 and 50000000")?;
        bounded(self.max_wall_seconds, 10, 86_400, "max_wall_seconds must be between 10 and 86400")?;
        Ok(self)
    }
}

fn bounded(value: Option<Option<i64>>, low: i64, high: i64, message: &'static str) -> std::result::Result<(), &'static str> {
    match value { Some(Some(n)) if !(low..=high).contains(&n) => Err(message), _ => Ok(()) }
}

/// `root_path` must be an absolute, existing directory outside the harness data directory
/// (which holds the database and is denied to every tool).
pub fn canonical_root(input: &str) -> std::result::Result<String, &'static str> {
    let raw = input.trim();
    if raw.is_empty() || raw.len() > 4096 || raw.chars().any(char::is_control) { return Err("root_path must be 1-4096 characters without control characters"); }
    let requested = std::path::Path::new(raw);
    if !requested.is_absolute() { return Err("root_path must be an absolute path"); }
    let canonical = std::fs::canonicalize(requested).map_err(|_| "root_path does not exist")?;
    if !canonical.is_dir() { return Err("root_path must be a directory"); }
    if canonical.parent().is_none() { return Err("root_path must not be the filesystem root"); }
    if crate::tools::paths::harness_data_dir().is_some_and(|data| canonical.starts_with(data)) {
        return Err("root_path must not be inside the harness data directory");
    }
    canonical.to_str().map(str::to_string).ok_or("root_path must be valid UTF-8")
}

impl DbStore {
    pub fn init(path: &str) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Inspect first: rejecting a legacy DB must not change its journal mode.
        let version:i64 = conn.query_row("PRAGMA user_version",[],|r|r.get(0))?;
        if version == 0 {
            let existing:i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",[],|r|r.get(0))?;
            if existing != 0 { bail!("legacy or unknown database: use scripts/migrate_legacy.py into a NEW database"); }
        } else if !(1..=3).contains(&version) { bail!("unsupported schema version {version}"); }
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
        if version == 0 { conn.execute_batch(include_str!("../migrations/001_core.sql"))?; }
        if version < 2 { conn.execute_batch(include_str!("../migrations/002_recording.sql"))?; }
        if version < 3 { conn.execute_batch(include_str!("../migrations/003_agentic.sql"))?; }
        // One process only. Never silently repeat a potentially billed generation.
        crate::recording::recover(&mut conn)?;
        Ok(Self{conn:Arc::new(Mutex::new(conn)),permits:Arc::new(Semaphore::new(32))})
    }
    pub async fn run<T,F>(&self, f:F) -> Result<T>
    where T:Send+'static, F:FnOnce(&mut Connection)->Result<T>+Send+'static {
        let permit = self.permits.clone().try_acquire_owned().map_err(|_|anyhow::anyhow!("database queue full"))?;
        let shared = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let _permit=permit;
            let mut conn=shared.lock().map_err(|_|anyhow::anyhow!("database lock poisoned"))?;
            f(&mut conn)
        }).await?
    }
    pub async fn settings(&self) -> Result<Value> {
        self.run(|c| {
            let mut map=serde_json::Map::new();
            for role in ["main","extraction"] {
                let value:Option<String>=c.query_row("SELECT value FROM settings WHERE key=?1",[format!("model.{role}")],|r|r.get(0)).optional()?;
                map.insert(role.into(),json!(value.unwrap_or_default()));
            }
            Ok(Value::Object(map))
        }).await
    }
    pub async fn set_settings(&self, data:std::collections::BTreeMap<String,String>) -> Result<()> {
        for (role,value) in &data { if !["main","extraction"].contains(&role.as_str()) || value.len()>128 || value.chars().any(char::is_control) { bail!("invalid model setting"); } }
        self.run(move|c|{ let tx=c.transaction()?; for (role,value) in data {tx.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![format!("model.{role}"),value.trim()])?;} tx.commit()?; Ok(()) }).await
    }
    pub async fn role_model(&self, role:&str, default:&str) -> Result<String> {
        let key=format!("model.{role}"); let default=default.to_string();
        self.run(move|c|{let v:Option<String>=c.query_row("SELECT value FROM settings WHERE key=?1",[key],|r|r.get(0)).optional()?; Ok(v.filter(|s|!s.is_empty()).unwrap_or(default))}).await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn ingest(&self,scope:String,name:String,format:String,content:String,fingerprint:String,warnings:Vec<String>,chunks:Vec<Vec<Event>>)->Result<Value>{
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
    pub async fn claim_job(&self)->Result<Option<Job>>{
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
    pub async fn finish_job(&self,job:Job,proposals:Vec<Proposal>)->Result<usize>{
        // Validate the entire result before storing ANY proposals.
        if proposals.len()>10 {bail!("too many extraction proposals");}
        for p in &proposals {
            safety::validate_fact(&p.key,&p.value,&p.category)?;
            if p.quote.trim().is_empty() || p.quote.chars().count()>1000 || !job.events.iter().any(|e| e.id==p.evidence_id && e.role=="user" && e.content.contains(&p.quote)) {bail!("proposal has invalid user evidence");}
        }
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;let mut count=0;
            for p in proposals {
                let revision:i64=tx.query_row("SELECT revision FROM memories WHERE scope=?1 AND key=?2",params![job.scope,p.key],|r|r.get(0)).optional()?.unwrap_or(0);
                let evidence=json!({"source_id":job.source_id,"event_id":p.evidence_id,"quote":p.quote});
                count+=tx.execute("INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending',?9,?10) ON CONFLICT(scope,key,value,source_id) DO NOTHING",params![uid(),job.scope,p.key,p.value,p.category,job.source_id,evidence.to_string(),revision,now(),Utc::now().timestamp()+30*86400])?;
            }
            tx.execute("UPDATE jobs SET status='done',last_error=NULL WHERE id=?1",[job.id])?;
            tx.commit()?;Ok(count)
        }).await
    }
    pub async fn fail_job(&self,id:String,attempts:i64)->Result<()> {
        self.run(move|c|{c.execute("UPDATE jobs SET status=?1,available_at=?2,last_error='extraction or validation failed; inspect provider configuration and retry' WHERE id=?3",params![if attempts>=3{"failed"}else{"pending"},Utc::now().timestamp()+30*attempts,id])?;Ok(())}).await
    }
    pub async fn retry_job(&self,id:String)->Result<bool>{
        self.run(move|c|Ok(c.execute("UPDATE jobs SET status='pending',attempts=0,available_at=?1,last_error=NULL WHERE id=?2 AND status='failed'",params![Utc::now().timestamp(),id])?==1)).await
    }
    pub async fn candidates(&self,scope:String)->Result<Value>{
        self.run(move|c|{
            c.execute("UPDATE candidates SET status='expired',resolved_at=?1 WHERE status='pending' AND expires_at<=?2",params![now(),Utc::now().timestamp()])?;
            let mut stmt=c.prepare("SELECT c.id,c.scope,c.key,c.value,c.category,c.evidence,c.expected_revision,m.value FROM candidates c LEFT JOIN memories m ON m.scope=c.scope AND m.key=c.key WHERE c.scope=?1 AND c.status='pending' ORDER BY c.created_at LIMIT 100")?;
            let rows=stmt.query_map([scope],|r|Ok(json!({"id":r.get::<_,String>(0)?,"scope":r.get::<_,String>(1)?,"key":r.get::<_,String>(2)?,"value":r.get::<_,String>(3)?,"category":r.get::<_,String>(4)?,"evidence":serde_json::from_str::<Value>(&r.get::<_,String>(5)?).unwrap_or(Value::Null),"expected_revision":r.get::<_,i64>(6)?,"old_value":r.get::<_,Option<String>>(7)?})))?;
            Ok(json!({"candidates":rows.collect::<rusqlite::Result<Vec<_>>>()?}))
        }).await
    }
    pub async fn resolve(&self,id:String,scope:String,confirm:bool)->Result<String>{
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
    pub async fn recall(&self,scope:String,prompt:String)->Result<Vec<Recall>>{
        let query=safety::fts_query(&prompt);if query.is_empty(){return Ok(Vec::new());}
        self.run(move|c|{
            let mut stmt=c.prepare("SELECT m.id,m.scope,m.key,m.value,m.revision,c.evidence FROM memory_fts JOIN memories m ON m.rowid=memory_fts.rowid JOIN candidates c ON c.id=m.candidate_id WHERE memory_fts MATCH ?1 AND m.status='active' AND (m.scope=?2 OR m.scope='global') AND (m.scope=?2 OR NOT EXISTS(SELECT 1 FROM memories p WHERE p.scope=?2 AND p.key=m.key AND p.status='active')) ORDER BY (m.scope=?2) DESC,bm25(memory_fts),m.updated_at DESC LIMIT 6")?;
            let rows=stmt.query_map(params![query,scope],|r|Ok(Recall{id:r.get(0)?,scope:r.get(1)?,key:r.get(2)?,value:r.get(3)?,revision:r.get(4)?,evidence:serde_json::from_str(&r.get::<_,String>(5)?).unwrap_or(Value::Null)}))?;
            let mut out=Vec::new();let mut bytes=0;
            for item in rows {let item=item?;let length=serde_json::to_string(&item)?.len();if bytes+length<=6000 && !safety::sensitive(&item.value){bytes+=length;out.push(item);}}
            Ok(out)
        }).await
    }
    pub async fn stats(&self)->Result<Value>{
        self.run(|c|{
            let active:i64=c.query_row("SELECT count(*) FROM memories WHERE status='active'",[],|r|r.get(0))?;
            let pending:i64=c.query_row("SELECT count(*) FROM candidates WHERE status='pending' AND expires_at>?1",[Utc::now().timestamp()],|r|r.get(0))?;
            let sources:i64=c.query_row("SELECT count(*) FROM sources",[],|r|r.get(0))?;
            let queued:i64=c.query_row("SELECT count(*) FROM jobs WHERE status IN ('pending','running')",[],|r|r.get(0))?;
            let failed:i64=c.query_row("SELECT count(*) FROM jobs WHERE status='failed'",[],|r|r.get(0))?;
            Ok(json!({"active_memories":active,"pending_confirmations":pending,"sources_stored":sources,"queued_jobs":queued,"failed_jobs":failed}))
        }).await
    }
    pub async fn jobs(&self)->Result<Value>{
        self.run(|c|{let mut stmt=c.prepare("SELECT id,scope,source_id,status,attempts,last_error FROM jobs ORDER BY created_at DESC LIMIT 100")?;
            let rows=stmt.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"scope":r.get::<_,String>(1)?,"source_id":r.get::<_,String>(2)?,"status":r.get::<_,String>(3)?,"attempts":r.get::<_,i64>(4)?,"error":r.get::<_,Option<String>>(5)?})))?;
            Ok(json!({"jobs":rows.collect::<rusqlite::Result<Vec<_>>>()?}))
        }).await
    }
    /// The stored scope row, or `None` when the scope was never configured (API answers 404).
    pub async fn scope_config(&self,scope:String)->Result<Option<ScopeConfig>>{
        self.run(move|c|Ok(c.query_row(crate::agentic_sql::SCOPE_GET,[scope],scope_row).optional()?)).await
    }
    /// Merge a validated patch into the stored row so a partial POST never clears a column
    /// the caller did not mention; `created_at` survives every later update.
    pub async fn upsert_scope(&self,scope:String,patch:ScopePatch)->Result<ScopeConfig>{
        safety::scope(&scope)?;
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut next=tx.query_row(crate::agentic_sql::SCOPE_GET,[&scope],scope_row).optional()?.unwrap_or_else(||ScopeConfig::blank(&scope));
            if let Some(value)=patch.root_path {next.root_path=value;}
            if let Some(value)=patch.permission_mode {next.permission_mode=value;}
            if let Some(value)=patch.diagnostics_cmd {next.diagnostics_cmd=value;}
            if let Some(value)=patch.max_steps {next.max_steps=value;}
            if let Some(value)=patch.max_tool_bytes {next.max_tool_bytes=value;}
            if let Some(value)=patch.max_wall_seconds {next.max_wall_seconds=value;}
            tx.execute(crate::agentic_sql::SCOPE_UPSERT,params![next.scope,next.root_path,next.permission_mode,next.diagnostics_cmd,next.max_steps,next.max_tool_bytes,next.max_wall_seconds,now()])?;
            let stored=tx.query_row(crate::agentic_sql::SCOPE_GET,[&scope],scope_row)?;
            tx.commit()?;Ok(stored)
        }).await
    }
    /// The stored plan, for the `plan_updated` event and the UI's plan panel.
    pub async fn plan(&self,session_id:String)->Result<Value>{
        self.run(move|c|plan_rows(c,&session_id)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test] async fn import_is_idempotent_and_not_active() {
        let db=DbStore::init(":memory:").unwrap();let events=vec![Event{id:"event-1:part-0".into(),role:"user".into(),content:"I prefer Rust".into()}];
        let a=db.ingest("global".into(),"test".into(),"claude".into(),"safe source".into(),"digest".into(),vec![],vec![events.clone()]).await.unwrap();
        assert_eq!(a["duplicate"],false);
        let b=db.ingest("global".into(),"test".into(),"claude".into(),"safe source".into(),"digest".into(),vec![],vec![events]).await.unwrap();assert_eq!(b["duplicate"],true);
        let job=db.claim_job().await.unwrap().unwrap();db.finish_job(job,vec![Proposal{key:"language".into(),value:"Rust".into(),category:"preference".into(),evidence_id:"event-1:part-0".into(),quote:"I prefer Rust".into()}]).await.unwrap();
        assert_eq!(db.stats().await.unwrap()["active_memories"],0);
        let list=db.candidates("global".into()).await.unwrap();let id=list["candidates"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(db.resolve(id.clone(),"wrong-scope".into(),true).await.unwrap(),"not_found");
        assert_eq!(db.resolve(id.clone(),"global".into(),true).await.unwrap(),"approved");
        assert_eq!(db.resolve(id,"global".into(),false).await.unwrap(),"already_resolved");
        assert_eq!(db.recall("global".into(),"Rust:".into()).await.unwrap().len(),1);
    }

    fn scope_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("harness-scope-{}", uid()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    #[tokio::test] async fn scopes_merge_partial_updates_and_gate_tools() {
        let db = DbStore::init(":memory:").unwrap();
        assert!(db.scope_config("global".into()).await.unwrap().is_none(), "an unconfigured scope must read as absent, not as defaults");
        let dir = scope_dir();
        let canonical = std::fs::canonicalize(&dir).unwrap();
        let patch = ScopePatch { root_path: Some(Some(dir.to_string_lossy().into())), permission_mode: Some("auto_edit".into()), diagnostics_cmd: Some(Some("  cargo check -q  ".into())), max_steps: Some(Some(12)), ..Default::default() }.validate().unwrap();
        let saved = db.upsert_scope("global".into(), patch).await.unwrap();
        assert_eq!(saved.root_path.as_deref(), canonical.to_str(), "root_path is stored canonicalized");
        assert_eq!(saved.diagnostics_cmd.as_deref(), Some("cargo check -q"));
        assert_eq!(saved.mode(), crate::tools::PermissionMode::AutoEdit);
        assert_eq!(saved.budgets(), (12, DEFAULT_MAX_TOOL_BYTES, DEFAULT_MAX_WALL_SECONDS));
        assert!(saved.tool_ctx("request", "step").is_some());
        let touched = db.upsert_scope("global".into(), ScopePatch { permission_mode: Some("ask".into()), ..Default::default() }.validate().unwrap()).await.unwrap();
        assert_eq!((touched.root_path, touched.diagnostics_cmd, touched.max_steps, touched.created_at), (saved.root_path, saved.diagnostics_cmd, saved.max_steps, saved.created_at));
        let cleared = db.upsert_scope("global".into(), ScopePatch { root_path: Some(None), diagnostics_cmd: Some(None), ..Default::default() }.validate().unwrap()).await.unwrap();
        assert!(cleared.root_path.is_none() && cleared.diagnostics_cmd.is_none(), "an explicit null clears the column");
        assert!(cleared.tool_ctx("request", "step").is_none(), "a scope without root_path must not hand a working directory to any tool");
        std::fs::remove_dir_all(dir).ok();
    }
    #[test] fn scopes_reject_unusable_configuration() {
        let dir = scope_dir();
        let file = dir.join("Cargo.toml");
        std::fs::write(&file, "x").unwrap();
        let rejected = [
            ScopePatch { root_path: Some(Some("relative/dir".into())), ..Default::default() },
            ScopePatch { root_path: Some(Some(dir.join("missing").to_string_lossy().into())), ..Default::default() },
            ScopePatch { root_path: Some(Some(file.to_string_lossy().into())), ..Default::default() },
            ScopePatch { permission_mode: Some("root".into()), ..Default::default() },
            ScopePatch { diagnostics_cmd: Some(Some("x".repeat(513))), ..Default::default() },
            ScopePatch { diagnostics_cmd: Some(Some("cargo check; rm -rf /".into())), ..Default::default() },
            ScopePatch { max_steps: Some(Some(0)), ..Default::default() },
            ScopePatch { max_tool_bytes: Some(Some(64)), ..Default::default() },
            ScopePatch { max_wall_seconds: Some(Some(5)), ..Default::default() },
        ];
        for bad in rejected {
            assert!(bad.clone().validate().is_err(), "expected a rejection for {bad:?}");
        }
        assert!(canonical_root("/").is_err(), "the filesystem root is never a project root");
        std::fs::remove_dir_all(dir).ok();
    }
    #[tokio::test] async fn plans_are_replaced_whole_or_not_at_all() {
        let db = DbStore::init(":memory:").unwrap();
        db.run(|c| { c.execute("INSERT INTO sessions VALUES('s','global','now')", [])?; Ok(()) }).await.unwrap();
        assert!(db.plan("s".into()).await.unwrap()["items"].as_array().unwrap().is_empty(), "a session starts without a plan");
        let first = replace(&db, vec![("  read the failing test  ".into(), "done".into()), ("fix the anchor".into(), "in_progress".into())]).await.unwrap();
        assert_eq!(first["items"][0]["text"], "read the failing test");
        assert_eq!((&first["items"][1]["seq"], &first["items"][1]["status"]), (&json!(2), &json!("in_progress")));
        let second = replace(&db, vec![("ship it".into(), "pending".into())]).await.unwrap();
        assert_eq!(second["items"].as_array().unwrap().len(), 1, "a replacement plan must not merge with the previous one");
        for bad in [
            vec![("a".into(), "in_progress".into()), ("b".into(), "in_progress".into())],
            vec![("  ".into(), "pending".into())],
            vec![("x".repeat(MAX_PLAN_TEXT + 1), "pending".into())],
            vec![("x".into(), "blocked".into())],
            (0..=MAX_PLAN_ITEMS).map(|i| (format!("step {i}"), "pending".to_string())).collect::<Vec<_>>(),
        ] {
            assert!(replace(&db, bad).await.is_err());
        }
        assert_eq!(db.plan("s".into()).await.unwrap()["items"], second["items"], "a refused plan leaves the stored one untouched");
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
        }).await
    }
}
