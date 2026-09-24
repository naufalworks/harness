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
pub const CURRENT_DATABASE_SCHEMA_VERSION: i64 = 23;
#[derive(Clone)]
pub struct DbStore {
    conn: Arc<Mutex<Connection>>,
    read_conns: Arc<Mutex<VecDeque<Connection>>>,
    pub commit_notify: tokio::sync::broadcast::Sender<()>,
    permits: Arc<Semaphore>,
    /// Identity of this process as a worker. Minted once per start: a restarted process must not
    /// be able to present the previous run's identity, or a lease would survive the crash of the
    /// worker that took it.
    worker: leases::WorkerIdentity,
    /// Leases this process currently believes it holds, keyed by request. A durable write is
    /// authorized by re-checking the remembered fence against the stored one inside the writing
    /// transaction, so a holder whose lease was taken over is refused rather than merged.
    held_leases: Arc<Mutex<std::collections::HashMap<String, leases::Lease>>>,
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
/// P15-T02: one considered memory and why retrieval kept or dropped it. The scores are the
/// ranking terms recall actually summed, not a re-derivation, and `reason` names the rule that
/// decided the outcome. It carries no claim about how a model used the memory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecallCandidate {
    pub id: String,
    pub scope: String,
    pub key: String,
    pub revision: i64,
    pub rank: i64,
    pub decision: String,
    pub reason: String,
    pub lexical_score: f64,
    pub semantic_score: f64,
    pub scope_score: f64,
    pub recency_score: f64,
    pub usefulness_score: f64,
    pub total_score: f64,
    pub bytes: i64,
}
impl RecallCandidate {
    pub fn as_value(&self) -> Value {
        json!({
            "memory_id":self.id,
            "scope":self.scope,
            "key":self.key,
            "revision":self.revision,
            "rank":self.rank,
            "decision":self.decision,
            "reason":self.reason,
            "lexical_score":self.lexical_score,
            "semantic_score":self.semantic_score,
            "scope_score":self.scope_score,
            "recency_score":self.recency_score,
            "usefulness_score":self.usefulness_score,
            "total_score":self.total_score,
            "bytes":self.bytes,
        })
    }
    /// The budget decision belongs to the context builder, so recall's row is amended rather
    /// than rewritten: the scores stay, the outcome becomes the one that shipped.
    pub fn excluded_by_context_budget(&mut self) {
        self.decision = "excluded".into();
        self.reason = "category_budget".into();
    }
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
mod causal_coverage;
mod config;
mod effects;
pub(crate) mod external_history;
mod history;
mod incident_compare;
mod jobs;
mod leases;
mod memories;
mod provenance;
mod provider;
mod turns;
pub use causal_coverage::{DeploymentAnomalies, MAX_COVERAGE_REQUESTS};
pub use effects::{EffectOutcome, ExternalEffectReservation};
pub use incident_compare::MAX_COMPARED_RUNS;
pub use leases::{
    acquire_in_tx, guard_fence, steal_in_tx, AcquireRefusal, Lease, LEASE_TTL_SECONDS,
};
pub use provenance::IncidentQuery;

/// The projection identity every incident-derived artifact reports, read from the single
/// P16-T01 projection rather than re-declared per consumer.
pub(crate) fn provenance_projection() -> &'static str {
    provenance::PROJECTION
}
mod scope;
pub use history::SearchScope;
pub use scope::*;

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
        } else if !(1..=CURRENT_DATABASE_SCHEMA_VERSION).contains(&version) {
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
        if version < 11 {
            conn.execute_batch(include_str!("../migrations/011_retrieval_receipts.sql"))?;
        }
        if version < 12 {
            conn.execute_batch(include_str!("../migrations/012_memory_governance.sql"))?;
        }
        if version < 13 {
            conn.execute_batch(include_str!("../migrations/013_session_workflows.sql"))?;
        }
        if version < 14 {
            conn.execute_batch(include_str!("../migrations/014_history_search.sql"))?;
        }
        if version < 15 {
            conn.execute_batch(include_str!("../migrations/015_causal_coverage.sql"))?;
        }
        if version < 16 {
            conn.execute_batch(include_str!("../migrations/016_run_capsules.sql"))?;
        }
        if version < 17 {
            conn.execute_batch(include_str!("../migrations/017_worker_leases.sql"))?;
        }
        if version < 18 {
            conn.execute_batch(include_str!("../migrations/018_external_effects.sql"))?;
        }
        if version < 19 {
            conn.execute_batch(include_str!("../migrations/019_tool_effect_kinds.sql"))?;
        }
        if version < 20 {
            conn.execute_batch(include_str!(
                "../migrations/020_archive_delete_outcomes.sql"
            ))?;
        }
        if version < 21 {
            conn.execute_batch(include_str!("../migrations/021_external_history.sql"))?;
        }
        if version < 22 {
            conn.execute_batch(include_str!("../migrations/022_agent_memory_protocol.sql"))?;
        }
        if version < 23 {
            conn.execute_batch(include_str!("../migrations/023_agent_session_links.sql"))?;
        }
        conn.execute(
            "UPDATE provider_calls SET state='failed',usage_status='unavailable',reason='process_restarted_with_call_reserved',finished_at=?1 WHERE state='reserved'",
            [Utc::now().to_rfc3339()],
        )?;
        // P18-T03: a still-reserved external effect means the process died between recording the
        // attempt and learning its outcome. That outcome is genuinely unknown, so it is recorded
        // as unknown for a human to decide rather than assumed failed and silently retried.
        conn.execute(
            "UPDATE external_effects SET state='unknown',reason='process_restarted_with_effect_reserved',settled_at=?1 WHERE state='reserved'",
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
            worker: leases::WorkerIdentity::for_this_process(),
            held_leases: Arc::new(Mutex::new(std::collections::HashMap::new())),
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
    pub async fn record_memory_health_baseline(&self) -> Result<()> {
        self.run(|c| {
            let count: i64 = c.query_row("SELECT count(*) FROM memories", [], |r| r.get(0))?;
            if count > 0 {
                c.execute(
                    "INSERT INTO settings(key,value) VALUES('memory_health_baseline',?1)
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value
                     WHERE CAST(settings.value AS INTEGER)<=0",
                    [count.to_string()],
                )?;
            }
            Ok(())
        })
        .await
    }
    /// P20 memory health guard: exposes memory volume and detects an unexpected reset signal.
    pub async fn memory_health(&self) -> Result<Value> {
        self.read(|c| {
            let memories:i64 = c.query_row("SELECT count(*) FROM memories", [], |r| r.get(0))?;
            let revisions:i64 = c.query_row("SELECT count(*) FROM memory_revisions", [], |r| r.get(0))?;
            let embeddings:i64 = c.query_row("SELECT count(*) FROM memory_embeddings", [], |r| r.get(0))?;
            let baseline:Option<i64> = c.query_row("SELECT value FROM settings WHERE key='memory_health_baseline'", [], |r| r.get::<_, String>(0).map(|v| v.parse().unwrap_or(0))).optional()?;
            let status = match baseline {
                Some(v) if v > 0 && memories == 0 => "MEMORY_RESET_DETECTED",
                None | Some(0) if memories == 0 => "NO_BASELINE",
                _ => "OK",
            };
            Ok(json!({"memories":memories,"memory_revisions":revisions,"memory_embeddings":embeddings,"previous_memory_baseline":baseline,"status":status}))
        }).await
    }
    /// P20 agent-neutral continuation envelope. Agents only contribute an identity; the stored
    /// session remains the source of truth.
    pub async fn continuation_context(
        &self,
        session_id: String,
        agent_id: String,
        agent_session_id: String,
    ) -> Result<Value> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let session: Option<(String,String,String,String)> = tx.query_row(
                "SELECT id,scope,created_at,title FROM sessions WHERE id=?1",
                [&session_id],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
            ).optional()?;
            let Some((id,scope,created_at,title)) = session else {
                bail!("unknown session");
            };
            let existing: Option<String> = tx.query_row(
                "SELECT harness_session_id FROM agent_session_links WHERE agent_id=?1 AND agent_session_id=?2",
                params![agent_id,agent_session_id],
                |r| r.get(0),
            ).optional()?;
            if let Some(existing) = existing {
                if existing != id {
                    bail!("agent session is already linked to a different Harness session");
                }
                tx.execute(
                    "UPDATE agent_session_links SET last_seen_at=?1 WHERE agent_id=?2 AND agent_session_id=?3",
                    params![now(),agent_id,agent_session_id],
                )?;
            } else {
                let stamp=now();
                tx.execute(
                    "INSERT INTO agent_session_links(id,agent_id,agent_session_id,harness_session_id,scope,created_at,last_seen_at) VALUES(?1,?2,?3,?4,?5,?6,?6)",
                    params![uid(),agent_id,agent_session_id,id,scope,stamp],
                )?;
            }
            let current_task: Option<Value> = tx.query_row(
                "SELECT content,created_at FROM messages WHERE session_id=?1 AND role='user' ORDER BY seq DESC LIMIT 1",
                [&id],
                |r| Ok(json!({"content":r.get::<_,String>(0)?,"created_at":r.get::<_,String>(1)?})),
            ).optional()?;
            let plan = {
                let mut stmt=tx.prepare("SELECT seq,text,status,updated_at FROM plan_items WHERE session_id=?1 ORDER BY seq LIMIT 100")?;
                let rows=stmt.query_map([&id],|r|Ok(json!({"seq":r.get::<_,i64>(0)?,"text":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"updated_at":r.get::<_,String>(3)?})))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            let mut blockers = {
                let mut stmt=tx.prepare("SELECT seq,text,status,updated_at FROM plan_items WHERE session_id=?1 AND status='failed' ORDER BY seq LIMIT 50")?;
                let rows=stmt.query_map([&id],|r|Ok(json!({"kind":"plan","seq":r.get::<_,i64>(0)?,"text":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"updated_at":r.get::<_,String>(3)?})))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            {
                let mut stmt=tx.prepare("SELECT request_id,state,error_code,updated_at FROM chat_receipts WHERE session_id=?1 AND state IN ('failed','interrupted') ORDER BY updated_at DESC LIMIT 20")?;
                blockers.extend(stmt.query_map([&id],|r|Ok(json!({"kind":"turn","request_id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"error_code":r.get::<_,Option<String>>(2)?,"updated_at":r.get::<_,String>(3)?})))?
                    .collect::<rusqlite::Result<Vec<_>>>()?);
            }
            let files_changed = {
                let mut stmt=tx.prepare(
                    "SELECT f.path,f.action,f.request_id,f.created_at FROM file_changes f
                     JOIN chat_receipts r ON r.request_id=f.request_id
                     WHERE r.session_id=?1 AND f.applied=1 AND f.reverted_at IS NULL
                     ORDER BY f.created_at DESC LIMIT 100"
                )?;
                let rows=stmt.query_map([&id],|r|Ok(json!({"path":r.get::<_,String>(0)?,"action":r.get::<_,String>(1)?,"request_id":r.get::<_,String>(2)?,"created_at":r.get::<_,String>(3)?})))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            let branch=memories::active_branch(&tx,&scope)?;
            let now_unix=Utc::now().timestamp();
            let visible_memories = {
                let mut stmt=tx.prepare(
                    "SELECT m.scope,m.key,m.value,m.category,m.revision,m.branch,m.conflict_group,m.updated_at
                     FROM memories m
                     WHERE m.status='active'
                       AND m.branch IN ('main',?2)
                       AND (m.expires_at IS NULL OR m.expires_at>?3)
                       AND NOT EXISTS(
                           SELECT 1 FROM memories b
                           WHERE b.scope=m.scope AND b.key=m.key AND b.branch=?2
                             AND b.status='active' AND (b.expires_at IS NULL OR b.expires_at>?3)
                             AND m.branch<>?2
                       )
                       AND (m.scope=?1 OR m.scope='global')
                       AND (m.scope=?1 OR NOT EXISTS(
                           SELECT 1 FROM memories p
                           WHERE p.scope=?1 AND p.key=m.key AND p.status='active'
                             AND p.branch IN ('main',?2)
                             AND (p.expires_at IS NULL OR p.expires_at>?3)
                       ))
                     ORDER BY CASE WHEN m.scope=?1 THEN 0 ELSE 1 END,m.key
                     LIMIT 200"
                )?;
                let rows=stmt.query_map(params![scope,branch,now_unix],|r|Ok(json!({
                    "scope":r.get::<_,String>(0)?,"key":r.get::<_,String>(1)?,"value":r.get::<_,String>(2)?,
                    "category":r.get::<_,String>(3)?,"revision":r.get::<_,i64>(4)?,"branch":r.get::<_,String>(5)?,
                    "conflict_group":r.get::<_,Option<String>>(6)?,"updated_at":r.get::<_,String>(7)?
                })))?.collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            let decisions: Vec<Value> = visible_memories.iter()
                .filter(|m|m["category"]=="decision").cloned().collect();
            let preferences: Vec<Value> = visible_memories.iter()
                .filter(|m|m["category"]=="preference").cloned().collect();
            let shared_project_memory: Vec<Value> = visible_memories.iter()
                .filter(|m|m["scope"]==scope).cloned().collect();
            let conflicts = {
                let mut stmt=tx.prepare(
                    "SELECT key,value,category,expected_revision,created_at FROM candidates
                     WHERE scope=?1 AND status='conflict' ORDER BY created_at DESC LIMIT 50"
                )?;
                let rows=stmt.query_map([&scope],|r|Ok(json!({"key":r.get::<_,String>(0)?,"value":r.get::<_,String>(1)?,"category":r.get::<_,String>(2)?,"expected_revision":r.get::<_,i64>(3)?,"created_at":r.get::<_,String>(4)?})))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            let linked_agents = {
                let mut stmt=tx.prepare(
                    "SELECT agent_id,agent_session_id,created_at,last_seen_at FROM agent_session_links
                     WHERE harness_session_id=?1 ORDER BY last_seen_at DESC LIMIT 50"
                )?;
                let rows=stmt.query_map([&id],|r|Ok(json!({"agent_id":r.get::<_,String>(0)?,"agent_session_id":r.get::<_,String>(1)?,"created_at":r.get::<_,String>(2)?,"last_seen_at":r.get::<_,String>(3)?})))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            tx.commit()?;
            Ok(json!({
                "format_version":1,
                "agent_independent":true,
                "session":{"id":id,"scope":scope,"title":title,"created_at":created_at},
                "agent":{"id":agent_id,"session_id":agent_session_id},
                "current_task":current_task,
                "plan":plan,
                "decisions":decisions,
                "blockers":blockers,
                "files_changed":files_changed,
                "preferences":preferences,
                "shared_project_memory":shared_project_memory,
                "conflicts":conflicts,
                "linked_agents":linked_agents
            }))
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
            Ok(json!({"ready":schema_version==CURRENT_DATABASE_SCHEMA_VERSION&&quick_check=="ok","schema_version":schema_version,
                "quick_check":quick_check,"queue":{"jobs_pending":pending_jobs,"jobs_running":running_jobs,
                "jobs_failed":failed_jobs,"turns_waiting":waiting_turns,"turns_running":running_turns},
                "maintenance":{"journal_mode":journal_mode,"last_wal_checkpoint_at":last_checkpoint,
                "last_retention_at":last_retention,"last_compaction_at":last_compaction}}))
        }).await
    }
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
