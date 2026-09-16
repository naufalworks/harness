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
#[allow(unused_imports)]
pub use provenance::{IncidentQuery, PROVENANCE_NODE_KINDS, PROVENANCE_RELATIONS};

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
        } else if !(1..=20).contains(&version) {
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
            Ok(json!({"ready":schema_version==20&&quick_check=="ok","schema_version":schema_version,
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
    use crate::agent_loop::{NewStep, StepOutcome};
    use crate::memory_agents::{ModelUsage, SpendLimits};
    use crate::tools::Artifact;
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

    /// P18-T04: a TTL alone cannot stop a stalled worker -- it only says when someone else *may*
    /// take over. The fence is what refuses the stalled worker's writes, and it is re-checked in
    /// the same transaction as the write. Resuming mints a *higher* fence, so the number the
    /// worker was carrying before the lapse no longer authorizes anything. Expiry here is forced
    /// through database-issued times, because a worker's own clock must not be able to decide
    /// whether it still owns a turn.
    #[tokio::test]
    async fn a_lapsed_lease_refuses_its_holders_writes_and_resuming_mints_a_higher_fence() {
        let dir = std::env::temp_dir().join(format!("harness-leases-{}", uid()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("harness.db").to_string_lossy().to_string();
        let db = DbStore::init(&path).unwrap();
        db.run(|c| {
            c.execute(
                "INSERT INTO sessions(id,scope,created_at) VALUES('s1','proj',?1)",
                params![now()],
            )?;
            c.execute(
                "INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES('r1','s1','user','hi','complete',?1)",
                params![now()],
            )?;
            c.execute(
                "INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES('r1','s1','proj','m','sig',0,'generating',?1,?1)",
                params![now()],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        let acquire = |worker: &'static str| {
            let db = db.clone();
            async move {
                db.run(move |c| {
                    let tx = c.transaction()?;
                    let outcome = leases::acquire_in_tx(&tx, "r1", worker)?;
                    tx.commit()?;
                    Ok(outcome)
                })
                .await
                .unwrap()
            }
        };
        let guard = |lease: leases::Lease| {
            let db = db.clone();
            async move {
                db.run(move |c| {
                    let tx = c.transaction()?;
                    let refusal = leases::guard_fence(&tx, &lease)
                        .err()
                        .map(|e| e.to_string());
                    tx.commit()?;
                    Ok(refusal)
                })
                .await
                .unwrap()
            }
        };

        let first = acquire("worker-a")
            .await
            .expect("a fresh turn has no holder");
        assert_eq!(first.fence, 1);
        assert_eq!(guard(first.clone()).await, None);
        assert!(db.renew_lease(&first).await.unwrap());

        // Age the lease past its expiry using the database's clock, not the test's.
        db.run(|c| {
            c.execute(
                "UPDATE worker_leases SET acquired_at=strftime('%Y-%m-%dT%H:%M:%fZ','now','-60 seconds'),renewed_at=strftime('%Y-%m-%dT%H:%M:%fZ','now','-60 seconds'),expires_at=strftime('%Y-%m-%dT%H:%M:%fZ','now','-30 seconds') WHERE request_id='r1'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        // A heartbeat cannot resurrect a lapsed lease; that would make expiry meaningless.
        assert!(!db.renew_lease(&first).await.unwrap());
        let lapsed = guard(first.clone())
            .await
            .expect("a lapsed lease must not authorize a write");
        assert!(lapsed.contains("lapsed"), "{lapsed}");

        // `acquire` never takes a turn away from another worker, lapsed or not; it names the
        // holder. Takeover is `steal_in_tx`, covered by the two tests below.
        assert_eq!(
            acquire("worker-b").await.unwrap_err(),
            leases::AcquireRefusal::HeldByAnother {
                worker_id: "worker-a".into(),
                lapsed: true
            }
        );

        let resumed = acquire("worker-a")
            .await
            .expect("its own lease is resumable");
        assert_eq!(resumed.fence, 2);
        assert_eq!(guard(resumed).await, None);
        // The pre-lapse fence is dead even though the same worker resumed: a write still in flight
        // from before the lapse is refused, and the refusal says who owns the turn now.
        let stale = guard(first)
            .await
            .expect("the old fence must not authorize a write");
        assert!(stale.contains("fence 2"), "{stale}");

        // Re-acquiring a lease this worker still holds is allowed, but it does not hand back the
        // same number: a write started under the previous fence may still be in flight, and two
        // live authorizations on one turn is the thing being prevented.
        let reacquired = acquire("worker-a")
            .await
            .expect("a worker may re-acquire its own lease");
        assert_eq!(reacquired.fence, 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    async fn leased_turn() -> (std::path::PathBuf, DbStore) {
        let dir = std::env::temp_dir().join(format!("harness-steal-{}", uid()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("harness.db").to_string_lossy().to_string();
        let db = DbStore::init(&path).unwrap();
        db.run(|c| {
            c.execute(
                "INSERT INTO sessions(id,scope,created_at) VALUES('s1','proj',?1)",
                params![now()],
            )?;
            c.execute(
                "INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES('r1','s1','user','hi','complete',?1)",
                params![now()],
            )?;
            c.execute(
                "INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES('r1','s1','proj','m','sig',0,'generating',?1,?1)",
                params![now()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        (dir, db)
    }

    async fn acquire_lease(
        db: &DbStore,
        worker: &'static str,
    ) -> std::result::Result<leases::Lease, leases::AcquireRefusal> {
        db.run(move |c| {
            let tx = c.transaction()?;
            let outcome = leases::acquire_in_tx(&tx, "r1", worker)?;
            tx.commit()?;
            Ok(outcome)
        })
        .await
        .unwrap()
    }

    async fn steal_lease(
        db: &DbStore,
        worker: &'static str,
    ) -> std::result::Result<leases::Lease, leases::StealRefusal> {
        db.run(move |c| {
            let tx = c.transaction()?;
            let outcome = leases::steal_in_tx(&tx, "r1", worker)?;
            // Committed even on refusal: the effects sweep inside a refused steal is the evidence
            // an operator has to read, so rolling it back would discard the reason for refusing.
            tx.commit()?;
            Ok(outcome)
        })
        .await
        .unwrap()
    }

    async fn guard_lease(db: &DbStore, lease: leases::Lease) -> Option<String> {
        db.run(move |c| {
            let tx = c.transaction()?;
            let refusal = leases::guard_fence(&tx, &lease)
                .err()
                .map(|e| e.to_string());
            tx.commit()?;
            Ok(refusal)
        })
        .await
        .unwrap()
    }

    /// Age the lease past expiry using the database's own clock, never the test process's.
    async fn lapse(db: &DbStore) {
        db.run(|c| {
            c.execute(
                "UPDATE worker_leases SET acquired_at=strftime('%Y-%m-%dT%H:%M:%fZ','now','-60 seconds'),renewed_at=strftime('%Y-%m-%dT%H:%M:%fZ','now','-60 seconds'),expires_at=strftime('%Y-%m-%dT%H:%M:%fZ','now','-30 seconds') WHERE request_id='r1'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    }

    /// P18-T04: worker-owned step writes fail closed unless this process can present the exact
    /// lease it acquired. Refusal happens before either half of the step/event pair is written.
    #[tokio::test]
    async fn durable_steps_without_a_remembered_lease_write_nothing() {
        let (dir, db) = leased_turn().await;
        let error = db
            .begin_step(NewStep {
                request: "r1".into(),
                session: "s1".into(),
                kind: "tool_call",
                tool_name: Some("read".into()),
                tool_call_id: Some("call-1".into()),
                input: json!({"path":"README.md"}),
                event: "tool_started",
                payload: json!({"tool":"read"}),
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("no remembered lease"), "{error}");
        assert_eq!(count(&db, "SELECT count(*) FROM turn_steps").await, 0);
        assert_eq!(count(&db, "SELECT count(*) FROM activity_events").await, 0);
        std::fs::remove_dir_all(dir).ok();
    }

    /// P18-T04: a worker that wakes after takeover must not finish its old running step. The
    /// rejected transaction leaves the step running and creates no event or file-change receipt.
    #[tokio::test]
    async fn a_stale_remembered_lease_cannot_finish_a_durable_step() {
        let (dir, db) = leased_turn().await;
        let stale = acquire_lease(&db, "worker-a")
            .await
            .expect("a fresh turn is claimable");
        db.remember_lease(stale);
        let step = db
            .begin_step(NewStep {
                request: "r1".into(),
                session: "s1".into(),
                kind: "tool_call",
                tool_name: Some("write".into()),
                tool_call_id: Some("call-1".into()),
                input: json!({"path":"note.txt"}),
                event: "tool_started",
                payload: json!({"tool":"write"}),
            })
            .await
            .unwrap();
        lapse(&db).await;
        let replacement = steal_lease(&db, "worker-b")
            .await
            .expect("a lapsed lease is stealable");
        assert_eq!(replacement.fence, 2);

        let error = db
            .finish_step(StepOutcome {
                step,
                request: "r1".into(),
                session: "s1".into(),
                status: "complete",
                output: json!({"ok":true}),
                bytes: 2,
                truncated: false,
                tokens_in: None,
                tokens_out: None,
                error_code: None,
                event: "tool_completed",
                payload: json!({"tool":"write"}),
                artifacts: vec![Artifact::FileChange {
                    path: "note.txt".into(),
                    action: "created",
                    before_hash: None,
                    after_hash: Some("a".repeat(64)),
                    diff: "+ok".into(),
                    plus: 1,
                    minus: 0,
                }],
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("fence 2"), "{error}");
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM turn_steps WHERE status='running'"
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM activity_events WHERE kind='tool_completed'"
            )
            .await,
            0
        );
        assert_eq!(count(&db, "SELECT count(*) FROM file_changes").await, 0);
        std::fs::remove_dir_all(dir).ok();
    }

    /// P18-T04 failure mode: ordinary SQLite write contention may delay a heartbeat, but a delay
    /// inside the configured busy timeout must not lose the lease, mint a new fence, or make the
    /// turn stealable. The competing writer is a separate connection so this exercises SQLite's
    /// lock rather than only this store's in-process mutex.
    #[tokio::test]
    async fn heartbeat_renewal_survives_write_contention_without_changing_the_fence() {
        let (dir, db) = leased_turn().await;
        let lease = acquire_lease(&db, "worker-a")
            .await
            .expect("a fresh turn has no holder");
        let before: String = db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT renewed_at FROM worker_leases WHERE request_id='r1'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();

        let path = dir.join("harness.db");
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let blocker = std::thread::spawn(move || {
            let mut competing = rusqlite::Connection::open(path).unwrap();
            competing
                .busy_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let tx = competing
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            locked_tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(250));
            tx.commit().unwrap();
        });
        locked_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the competing writer must hold the SQLite write lock");

        assert!(db.renew_lease(&lease).await.unwrap());
        blocker.join().unwrap();
        let (renewed, fence, holder, state): (String, i64, String, String) = db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT renewed_at,fence,worker_id,state FROM worker_leases WHERE request_id='r1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?)
            })
            .await
            .unwrap();
        assert!(
            renewed > before,
            "heartbeat did not advance: {before} -> {renewed}"
        );
        assert_eq!(
            (fence, holder.as_str(), state.as_str()),
            (1, "worker-a", "held")
        );
        assert_eq!(guard_lease(&db, lease).await, None);
        assert_eq!(
            steal_lease(&db, "worker-b").await.unwrap_err(),
            leases::StealRefusal::StillLive {
                worker_id: "worker-a".into()
            }
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P18-T04 failure mode: cancellation is durable intent in `run_controls`, not an ownership
    /// operation. A process that does not hold the turn may request and finalize cancellation, and
    /// doing so must neither acquire nor steal the holder's lease.
    #[tokio::test]
    async fn cancellation_at_a_non_holder_is_honored_without_moving_the_lease() {
        let (dir, db) = leased_turn().await;
        let holder = acquire_lease(&db, "worker-a")
            .await
            .expect("a fresh turn has no holder");
        assert_ne!(db.worker_identity().as_str(), holder.worker_id);
        assert!(
            db.held_lease("r1").is_none(),
            "the cancelling store is not the holder"
        );

        let requested = db
            .request_cancellation("r1".into())
            .await
            .unwrap()
            .expect("the generating turn exists");
        assert_eq!(requested["state"], "generating");
        let lease_row: (String, i64, String) = db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT worker_id,fence,state FROM worker_leases WHERE request_id='r1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(lease_row, ("worker-a".into(), 1, "held".into()));

        assert!(db.finalize_cancellation("r1".into()).await.unwrap());
        assert!(!db.finalize_cancellation("r1".into()).await.unwrap());
        let receipt = db.recording_receipt("r1".into()).await.unwrap().unwrap();
        assert_eq!(receipt["state"], "interrupted");
        assert_eq!(receipt["error_code"], "cancelled");
        let evidence: (i64, i64, i64, i64) = db
            .read(|c| {
                Ok((
                    c.query_row(
                        "SELECT count(*) FROM activity_events WHERE request_id='r1' AND kind='cancel_requested'",
                        [],
                        |r| r.get(0),
                    )?,
                    c.query_row(
                        "SELECT count(*) FROM activity_events WHERE request_id='r1' AND kind='turn_cancelled'",
                        [],
                        |r| r.get(0),
                    )?,
                    c.query_row(
                        "SELECT count(*) FROM generation_events WHERE request_id='r1' AND state='interrupted' AND error_code='cancelled'",
                        [],
                        |r| r.get(0),
                    )?,
                    c.query_row(
                        "SELECT count(*) FROM external_effects WHERE request_id='r1'",
                        [],
                        |r| r.get(0),
                    )?,
                ))
            })
            .await
            .unwrap();
        assert_eq!(evidence, (1, 1, 1, 0));
        let unchanged: (String, i64, String) = db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT worker_id,fence,state FROM worker_leases WHERE request_id='r1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(unchanged, lease_row);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P18-T04 failure mode: workers can disagree arbitrarily about wall time, but no worker time
    /// is accepted by the lease API. Acquisition, renewal, liveness and takeover are all bounded by
    /// SQLite's own UTC clock.
    #[tokio::test]
    async fn worker_clock_skew_cannot_extend_or_revoke_database_issued_ownership() {
        let (dir, db) = leased_turn().await;
        let absurdly_slow_worker = "1900-01-01T00:00:00.000Z";
        let absurdly_fast_worker = "9999-12-31T23:59:59.999Z";
        let db_before: String = db
            .read(|c| {
                Ok(
                    c.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .await
            .unwrap();
        let lease = acquire_lease(&db, "worker-a")
            .await
            .expect("worker-local time is not part of acquisition");
        assert!(db.renew_lease(&lease).await.unwrap());
        let db_after: String = db
            .read(|c| {
                Ok(
                    c.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .await
            .unwrap();
        let (acquired, renewed, expires): (String, String, String) = db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT acquired_at,renewed_at,expires_at FROM worker_leases WHERE request_id='r1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })
            .await
            .unwrap();
        assert!(db_before <= acquired && acquired <= db_after);
        assert!(db_before <= renewed && renewed <= db_after);
        assert!(expires > renewed);
        assert_ne!(renewed, absurdly_slow_worker);
        assert_ne!(renewed, absurdly_fast_worker);
        assert_eq!(guard_lease(&db, lease.clone()).await, None);
        assert_eq!(
            steal_lease(&db, "worker-b").await.unwrap_err(),
            leases::StealRefusal::StillLive {
                worker_id: "worker-a".into()
            }
        );

        // Once SQLite itself says the lease is past due, the same two skewed workers get the same
        // answer: the old fence is dead and takeover raises it exactly once.
        lapse(&db).await;
        let stolen = steal_lease(&db, "worker-b")
            .await
            .expect("database-issued expiry permits takeover");
        assert_eq!(stolen.fence, 2);
        assert!(guard_lease(&db, lease).await.is_some());
        assert_eq!(guard_lease(&db, stolen).await, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P18-T04: takeover. A lapsed lease does not prove the old holder is gone, only that it
    /// stopped reporting. Raising the fence is what actually disarms it: the number it remembers
    /// no longer matches the database, so a write it starts after waking up is refused rather
    /// than landed beside the new holder's.
    #[tokio::test]
    async fn stealing_a_lapsed_lease_raises_the_fence_and_disarms_the_old_holder() {
        let (dir, db) = leased_turn().await;
        let dead = acquire_lease(&db, "worker-a")
            .await
            .expect("a fresh turn has no holder");
        assert_eq!(dead.fence, 1);

        // A live lease is not stealable. Taking a working worker's turn is a race, not recovery.
        assert_eq!(
            steal_lease(&db, "worker-b").await.unwrap_err(),
            leases::StealRefusal::StillLive {
                worker_id: "worker-a".into()
            }
        );

        lapse(&db).await;
        let stolen = steal_lease(&db, "worker-b")
            .await
            .expect("a lapsed lease with no effect in flight is recoverable");
        assert_eq!(stolen.fence, 2);
        assert_eq!(guard_lease(&db, stolen).await, None);
        let refused = guard_lease(&db, dead)
            .await
            .expect("the old holder must no longer authorize writes");
        assert!(refused.contains("worker-b"), "{refused}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P18-T04: the steal-after-death case that actually matters. The lease says the old holder is
    /// gone; it says nothing about whether the provider call that holder had already dispatched
    /// reached the outside world. Handing the turn over there would ask the new worker to redo a
    /// paid effect. So the reservation is swept to `unknown`, the steal is refused, and the turn
    /// waits for a human -- decision 4 applied literally, with no auto-retry anywhere.
    #[tokio::test]
    async fn a_steal_is_refused_when_the_dead_holder_left_an_effect_in_flight() {
        let (dir, db) = leased_turn().await;
        let dead = acquire_lease(&db, "worker-a")
            .await
            .expect("a fresh turn has no holder");
        let effect = db
            .reserve_external_effect(
                "r1".into(),
                "call-1".into(),
                "d".repeat(64),
                "provider_call".into(),
                Some(dead.clone()),
            )
            .await
            .unwrap()
            .expect("the turn is recorded, so the effect is attributable");
        lapse(&db).await;

        assert_eq!(
            steal_lease(&db, "worker-b").await.unwrap_err(),
            leases::StealRefusal::EffectsNeedDecision {
                effect_ids: vec![effect.id().to_string()]
            }
        );
        // The refusal is not the whole guarantee; the durable evidence is.
        let looked_up = effect.id().to_string();
        let (state, reason): (String, Option<String>) = db
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT state,reason FROM external_effects WHERE effect_id=?1",
                    [&looked_up],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(state, "unknown");
        assert_eq!(
            reason.as_deref(),
            Some("lease_lapsed_with_effect_in_flight")
        );

        // And the effect cannot be quietly redone under a higher fence: the identity deliberately
        // excludes the fence, so a takeover could never pay for the same call twice.
        let again = db
            .reserve_external_effect(
                "r1".into(),
                "call-1".into(),
                "d".repeat(64),
                "provider_call".into(),
                Some(dead.clone()),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(
            again.contains("lease") || again.contains("lapsed"),
            "{again}"
        );

        // Ownership did not move: refusing a steal must leave the recorded holder and fence alone,
        // or the turn would look recovered while its effect is still unclassified.
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM worker_leases WHERE worker_id='worker-a' AND fence=1"
            )
            .await,
            1
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P18-T03: `provider_calls` records what a call cost; this records whether it may have
    /// happened, which is the question a restarted or stolen turn has to ask before retrying.
    /// The row is written before dispatch, settled once, and swept to `unknown` by init when the
    /// process died mid-flight -- the one outcome that owes a human a decision.
    #[tokio::test]
    async fn external_effects_are_recorded_before_dispatch_and_swept_to_unknown_on_restart() {
        let dir = std::env::temp_dir().join(format!("harness-effects-{}", uid()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("harness.db").to_string_lossy().to_string();
        let db = DbStore::init(&path).unwrap();
        db.run(|c| {
            c.execute(
                "INSERT INTO sessions(id,scope,created_at) VALUES('s1','proj',?1)",
                params![now()],
            )?;
            c.execute(
                "INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES('r1','s1','user','hi','complete',?1)",
                params![now()],
            )?;
            c.execute(
                "INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES('r1','s1','proj','m','sig',0,'generating',?1,?1)",
                params![now()],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        let lease = acquire_lease(&db, "worker-a")
            .await
            .expect("the recorded turn is claimable");

        let missing = db
            .reserve_external_effect(
                "r1".into(),
                "call-without-lease".into(),
                "c".repeat(64),
                "provider_call".into(),
                None,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(missing.contains("no remembered lease"), "{missing}");
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM external_effects WHERE step_identity='call-without-lease'"
            )
            .await,
            0
        );

        // An effect outside a recorded turn is left unrecorded, not refused: the row references
        // `chat_receipts`, and failing the insert would turn a bookkeeping gap into a refused call.
        assert!(db
            .reserve_external_effect(
                "not-a-recorded-turn".into(),
                "call-0".into(),
                "d".repeat(64),
                "provider_call".into(),
                None,
            )
            .await
            .unwrap()
            .is_none());

        let effect = db
            .reserve_external_effect(
                "r1".into(),
                "call-1".into(),
                "a".repeat(64),
                "provider_call".into(),
                Some(lease.clone()),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM external_effects WHERE step_identity='call-1' AND fence=1"
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM external_effects WHERE state='reserved'"
            )
            .await,
            1
        );
        // The same identity a second time is exactly the duplicate this table exists to prevent.
        assert!(db
            .reserve_external_effect(
                "r1".into(),
                "call-1".into(),
                "a".repeat(64),
                "provider_call".into(),
                Some(lease.clone()),
            )
            .await
            .is_err());

        let stranded = db
            .reserve_external_effect(
                "r1".into(),
                "call-2".into(),
                "b".repeat(64),
                "provider_call".into(),
                Some(lease.clone()),
            )
            .await
            .unwrap()
            .unwrap();
        db.settle_external_effect(
            effect.clone(),
            EffectOutcome::Succeeded,
            Some("response-1".into()),
            None,
        )
        .await
        .unwrap();
        // An outcome is recorded once; a later writer cannot rewrite it.
        assert!(db
            .settle_external_effect(effect, EffectOutcome::Failed, None, Some("second".into()))
            .await
            .is_err());
        // `unknown` with no reason is an alarm with nothing for a human to act on.
        assert!(db
            .settle_external_effect(stranded.clone(), EffectOutcome::Unknown, None, None)
            .await
            .is_err());
        drop(db);

        let reopened = DbStore::init(&path).unwrap();
        let looked_up = stranded.id().to_string();
        let swept: (String, Option<String>) = reopened
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT state,reason FROM external_effects WHERE effect_id=?1",
                    params![looked_up],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(swept.0, "unknown");
        assert_eq!(
            swept.1.as_deref(),
            Some("process_restarted_with_effect_reserved")
        );
        let owed = reopened.unknown_external_effects(10).await.unwrap();
        assert_eq!(owed.len(), 1);
        assert_eq!(owed[0]["effect_id"], json!(stranded.id()));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P18-T04: settlement is a durable turn write too. It must present the lease captured when
    /// the effect was reserved, not authorize itself by reading whatever fence is current later.
    #[tokio::test]
    async fn a_stale_holder_cannot_settle_an_external_effect_after_takeover() {
        let (dir, db) = leased_turn().await;
        let stale = acquire_lease(&db, "worker-a")
            .await
            .expect("a fresh turn is claimable");
        let effect = db
            .reserve_external_effect(
                "r1".into(),
                "call-1".into(),
                "d".repeat(64),
                "provider_call".into(),
                Some(stale),
            )
            .await
            .unwrap()
            .unwrap();
        db.run(|c| {
            c.execute(
                "UPDATE worker_leases SET worker_id='worker-b',fence=2,state='held',expires_at=strftime('%Y-%m-%dT%H:%M:%fZ','now','+30 seconds') WHERE request_id='r1'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        let refused = db
            .settle_external_effect(effect, EffectOutcome::Succeeded, None, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("worker-b") && refused.contains("fence 2"),
            "{refused}"
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM external_effects WHERE step_identity='call-1' AND state='reserved' AND fence=1"
            )
            .await,
            1
        );
        std::fs::remove_dir_all(&dir).ok();
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
        assert_eq!(readiness["schema_version"], 20);
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

    /// P15-T01: measure the real `DbStore::recall` path over labeled fixtures and write
    /// `tests/recall_eval/metrics.json`, which `tests/recall_eval/run.py --check` then validates.
    ///
    /// The measurement lives here rather than in Python on purpose: ranking is Rust, and a Python
    /// reimplementation would report on the copy instead of the shipped code. Each case gets its
    /// own in-memory database so one case cannot pollute another's candidate pool.
    #[tokio::test]
    async fn recall_eval_fixtures_meet_labeled_budgets() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/recall_eval");
        let raw = std::fs::read_to_string(dir.join("fixtures.json"))
            .expect("tests/recall_eval/fixtures.json must exist");
        let fixtures: Value = serde_json::from_str(&raw).expect("fixtures must be valid JSON");
        let thresholds = fixtures["thresholds"].clone();
        let mut measured = Vec::new();
        let (mut relevant_total, mut relevant_hit) = (0i64, 0i64);
        let (mut stale_total, mut stale_hit) = (0i64, 0i64);
        let mut precision_sum = 0.0f64;
        let (mut max_bytes, mut max_latency) = (0usize, 0u64);
        // The lexical-only arm is measured alongside the default so the operator can see the
        // precision/coverage trade before choosing a strategy, which is what "before enabling a
        // model" has to mean in practice.
        let (mut lex_precision_sum, mut lex_relevant_hit, mut lex_returned) =
            (0.0f64, 0i64, 0usize);

        for case in fixtures["cases"].as_array().expect("cases array") {
            let case_id = case["id"].as_str().expect("case id").to_string();
            let db = DbStore::init(":memory:").unwrap();
            let rows = case["memories"].as_array().expect("memories").clone();
            let seed = rows.clone();
            db.run(move |c| {
                for memory in &seed {
                    let id = memory["id"].as_str().unwrap();
                    let scope = memory["scope"].as_str().unwrap();
                    let key = memory["key"].as_str().unwrap();
                    let value = memory["value"].as_str().unwrap();
                    let category = memory["category"].as_str().unwrap();
                    let status = memory["status"].as_str().unwrap_or("active");
                    let age_days = memory["age_days"].as_i64().unwrap_or(0);
                    // Backdate `updated_at` so the recency term is genuinely exercised.
                    let stamp = (Utc::now() - chrono::Duration::days(age_days)).to_rfc3339();
                    let candidate = format!("c-{id}");
                    c.execute(
                        "INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at,resolved_at) VALUES(?1,?2,?3,?4,?5,?6,'{}',0,'approved',?7,2000000000,?7)",
                        params![candidate, scope, key, value, category, format!("source-{id}"), stamp],
                    )?;
                    c.execute(
                        "INSERT INTO memories(id,scope,key,value,category,status,revision,candidate_id,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,1,?7,?8,?8)",
                        params![id, scope, key, value, category, status, candidate, stamp],
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();

            let labels = rows
                .iter()
                .map(|memory| {
                    (
                        memory["id"].as_str().unwrap().to_string(),
                        memory["label"].as_str().unwrap_or("irrelevant").to_string(),
                    )
                })
                .collect::<std::collections::HashMap<_, _>>();
            let relevant_available = labels.values().filter(|label| *label == "relevant").count();
            let stale_available = labels.values().filter(|label| *label == "stale").count();

            let started = std::time::Instant::now();
            let recalled = db
                .recall_with_strategy(
                    case["scope"].as_str().unwrap().into(),
                    case["prompt"].as_str().unwrap().into(),
                    crate::embeddings::Strategy::Hybrid,
                )
                .await
                .unwrap();
            let latency_ms = started.elapsed().as_millis() as u64;

            // Same fixtures, lexical arm only. Pinned explicitly rather than read from the
            // environment so the recorded comparison cannot drift with ambient configuration.
            let lexical = db
                .recall_with_strategy(
                    case["scope"].as_str().unwrap().into(),
                    case["prompt"].as_str().unwrap().into(),
                    crate::embeddings::Strategy::LexicalOnly,
                )
                .await
                .unwrap();
            let lexical_relevant = lexical
                .iter()
                .filter(|memory| labels.get(&memory.id).map(String::as_str) == Some("relevant"))
                .count();
            lex_precision_sum += if lexical.is_empty() {
                if relevant_available == 0 {
                    1.0
                } else {
                    0.0
                }
            } else {
                lexical_relevant as f64 / lexical.len() as f64
            };
            lex_relevant_hit += lexical_relevant as i64;
            lex_returned += lexical.len();

            let relevant_recalled = recalled
                .iter()
                .filter(|memory| labels.get(&memory.id).map(String::as_str) == Some("relevant"))
                .count();
            let stale_recalled = recalled
                .iter()
                .filter(|memory| labels.get(&memory.id).map(String::as_str) == Some("stale"))
                .count();
            let context_bytes = recalled
                .iter()
                .map(|memory| serde_json::to_vec(memory).unwrap().len())
                .sum::<usize>();
            // An empty result is perfectly precise only when nothing relevant existed to find.
            let precision = if recalled.is_empty() {
                if relevant_available == 0 {
                    1.0
                } else {
                    0.0
                }
            } else {
                relevant_recalled as f64 / recalled.len() as f64
            };

            relevant_total += relevant_available as i64;
            relevant_hit += relevant_recalled as i64;
            stale_total += stale_available as i64;
            stale_hit += stale_recalled as i64;
            precision_sum += precision;
            max_bytes = max_bytes.max(context_bytes);
            max_latency = max_latency.max(latency_ms);

            assert_eq!(
                stale_recalled, 0,
                "case {case_id}: archived/stale memories must never reach the context, got {recalled:#?}"
            );
            assert!(
                context_bytes <= thresholds["max_context_bytes"].as_u64().unwrap() as usize,
                "case {case_id}: context cost {context_bytes}B exceeds the declared ceiling"
            );

            measured.push(json!({
                "id": case_id,
                "precision": precision,
                "relevant_recalled": relevant_recalled,
                "relevant_available": relevant_available,
                "stale_recalled": stale_recalled,
                "stale_available": stale_available,
                "returned": recalled.len(),
                "context_bytes": context_bytes,
                "latency_ms": latency_ms,
            }));
        }

        let case_count = measured.len();
        assert!(case_count > 0, "fixtures declared no cases");
        let macro_precision = precision_sum / case_count as f64;
        let relevant_coverage = if relevant_total == 0 {
            1.0
        } else {
            relevant_hit as f64 / relevant_total as f64
        };
        let stale_use_rate = if stale_total == 0 {
            0.0
        } else {
            stale_hit as f64 / stale_total as f64
        };

        let metrics = json!({
            "model": crate::embeddings::MODEL,
            "dimensions": crate::embeddings::DIMENSIONS,
            "generated_by": "storage::tests::recall_eval_fixtures_meet_labeled_budgets",
            "fixture_sha256": crate::safety::fingerprint(&raw),
            "fixture_case_count": case_count,
            "cases": measured,
            "totals": {
                "macro_precision": macro_precision,
                "relevant_coverage": relevant_coverage,
                "stale_use_rate": stale_use_rate,
                "max_context_bytes": max_bytes,
                "max_latency_ms": max_latency,
            },
            "strategy": "hybrid",
            "comparison": {
                "lexical_only": {
                    "macro_precision": lex_precision_sum / case_count as f64,
                    "relevant_coverage": if relevant_total == 0 {
                        1.0
                    } else {
                        lex_relevant_hit as f64 / relevant_total as f64
                    },
                    "returned": lex_returned,
                },
                "opt_out_env": crate::embeddings::STRATEGY_ENV,
            },
        });
        std::fs::write(
            dir.join("metrics.json"),
            format!("{}\n", serde_json::to_string_pretty(&metrics).unwrap()),
        )
        .expect("metrics.json must be writable");

        // Assert here too, so `cargo test` alone already fails on a quality regression rather
        // than relying on the Python gate that runs afterwards.
        assert!(
            macro_precision >= thresholds["min_macro_precision"].as_f64().unwrap(),
            "macro precision {macro_precision:.3} is below the declared budget"
        );
        assert!(
            relevant_coverage >= thresholds["min_relevant_coverage"].as_f64().unwrap(),
            "relevant coverage {relevant_coverage:.3} is below the declared budget"
        );
        assert!(
            stale_use_rate <= thresholds["max_stale_use_rate"].as_f64().unwrap(),
            "stale use rate {stale_use_rate:.3} exceeds the declared budget"
        );
    }

    /// P15-T01: the opt-out must change retrieval, not just configuration. A prompt that shares
    /// no lexical token with any memory still clears the vector arm's cosine floor on noise
    /// alone; with the vector arm off, it must return nothing.
    #[tokio::test]
    async fn recall_lexical_only_strategy_suppresses_vector_noise() {
        let db = DbStore::init(":memory:").unwrap();
        db.run(|c| {
            let stamp = now();
            for (id, key, value) in [
                ("m-db", "database", "SQLite"),
                ("m-lang", "language", "Rust"),
            ] {
                c.execute(
                    "INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at,resolved_at) VALUES(?1,'proj',?2,?3,'project',?4,'{}',0,'approved',?5,2000000000,?5)",
                    params![format!("c-{id}"), key, value, format!("s-{id}"), stamp],
                )?;
                c.execute(
                    "INSERT INTO memories(id,scope,key,value,category,status,revision,candidate_id,created_at,updated_at) VALUES(?1,'proj',?2,?3,'project','active',1,?4,?5,?5)",
                    params![id, key, value, format!("c-{id}"), stamp],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();

        let nonsense = "xylophone quarterly submarine";
        let hybrid = db
            .recall_with_strategy(
                "proj".into(),
                nonsense.into(),
                crate::embeddings::Strategy::Hybrid,
            )
            .await
            .unwrap();
        assert!(
            !hybrid.is_empty(),
            "baseline: the vector arm admits unrelated memories on hash noise, which is the \
             measured behaviour this strategy exists to let operators avoid"
        );

        let lexical = db
            .recall_with_strategy(
                "proj".into(),
                nonsense.into(),
                crate::embeddings::Strategy::LexicalOnly,
            )
            .await
            .unwrap();
        assert!(
            lexical.is_empty(),
            "lexical-only must admit nothing without lexical support, got {lexical:#?}"
        );

        // The opt-out must not cost genuine lexical hits.
        let supported = db
            .recall_with_strategy(
                "proj".into(),
                "which database".into(),
                crate::embeddings::Strategy::LexicalOnly,
            )
            .await
            .unwrap();
        assert!(
            supported.iter().any(|memory| memory.id == "m-db"),
            "lexical matches must still be recalled, got {supported:#?}"
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

    /// P16-T01. One fixture, four claims: the default query still reproduces the
    /// pre-P16-T01 shape; filters narrow the same projection instead of a second one;
    /// the chronological view orders by recorded time and never invents one; and a node
    /// with no recorded edge is labelled by proximity, never upgraded to a dependency.
    async fn incident_fixture() -> DbStore {
        let db = DbStore::init(":memory:").unwrap();
        db.run(|c| {
            let stamp = now();
            c.execute("INSERT INTO sessions(id,scope,created_at) VALUES('session','global',?1)",[&stamp])?;
            c.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES('request','session','user','hi','pending',?1)",[&stamp])?;
            c.execute("INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES('request','session','global','main','sig',0,'failed',?1,?1)",[&stamp])?;
            // Two steps with different tools; only the first is wired to a permission by a
            // recorded edge, so the second must come back as temporal proximity.
            c.execute(crate::agentic_sql::STEP_BEGIN,params!["step-a","request",None::<String>,0,"tool_call","edit","call-a","{}",stamp])?;
            c.execute(crate::agentic_sql::STEP_BEGIN,params!["step-b","request",None::<String>,1,"tool_call","bash","call-b","{}",stamp])?;
            c.execute(crate::agentic_sql::PERMISSION_CREATE,params!["permit-a","request","step-a","edit","edit","{}",stamp,stamp])?;
            c.execute(crate::agentic_sql::PROVENANCE_EDGE_INSERT,params!["edge-a","request","step","step-a","depends_on","permission","permit-a",stamp])?;
            // A real same-scope memory endpoint (migration 006's trigger requires one). It is a
            // durable row that this projection does not read a timestamp for, so it resolves to
            // an undated node -- which is what makes the undated-row handling testable.
            c.execute("INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at) VALUES('cand-a','global','k','v','fact','src','{}',0,'approved',?1,0)",[&stamp])?;
            c.execute("INSERT INTO memories(id,scope,key,value,category,status,revision,candidate_id,created_at,updated_at) VALUES('mem-a','global','k','v','fact','active',1,'cand-a',?1,?1)",[&stamp])?;
            c.execute(crate::agentic_sql::PROVENANCE_EDGE_INSERT,params!["edge-b","request","memory","mem-a","supports","step","step-a",stamp])?;
            c.execute("INSERT INTO file_changes(id,request_id,step_id,path,action,diff,applied,created_at) VALUES('mut-a','request','step-a','src/lib.rs','modify','--- a\n+++ b\n',1,?1)",[&stamp])?;
            c.execute(crate::agentic_sql::STEP_FINISH,params!["step-b","failed","{}",0,0,None::<i64>,None::<i64>,"tool_failed",stamp])?;
            Ok(())
        }).await.unwrap();
        db
    }

    #[tokio::test]
    async fn incident_confidence_separates_recorded_dependency_from_proximity() {
        let db = incident_fixture().await;
        let graph = db.incident_graph("request".into()).await.unwrap().unwrap();
        let by_id = |id: &str| {
            graph["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|node| node["id"] == json!(id))
                .cloned()
                .unwrap_or_else(|| panic!("missing node {id}"))
        };
        // A recorded edge is the ONLY way to earn a dependency label.
        assert_eq!(by_id("step:step-a")["confidence"], "recorded_dependency");
        assert_eq!(by_id("step:step-a")["confidence_basis"], "provenance_edge");
        assert_eq!(
            by_id("permission:permit-a")["confidence"],
            "recorded_dependency"
        );
        // step-b co-occurs and has a recorded time, but nothing was recorded about it.
        assert_eq!(by_id("step:step-b")["confidence"], "temporal_proximity");
        assert_eq!(
            by_id("step:step-b")["confidence_basis"],
            "same_request_recorded_time"
        );
        assert_eq!(by_id("step:step-b")["provenance"], "unknown");
        // The legend ships with the response so a client cannot invent a fourth meaning.
        assert!(graph["confidence_labels"]["temporal_proximity"]
            .as_str()
            .unwrap()
            .contains("not causation"));
        // No field may carry model reasoning. Checked over keys, recursively: the prose note
        // legitimately contains the word "reasoning" while promising the absence of the thing.
        fn keys(value: &Value, out: &mut Vec<String>) {
            match value {
                Value::Object(map) => {
                    for (key, child) in map {
                        out.push(key.clone());
                        keys(child, out);
                    }
                }
                Value::Array(items) => items.iter().for_each(|item| keys(item, out)),
                _ => {}
            }
        }
        let mut names = Vec::new();
        keys(&graph, &mut names);
        for name in &names {
            for banned in ["thought", "reasoning", "rationale", "explanation"] {
                assert!(!name.contains(banned), "leaked key {name}");
            }
        }
        assert_eq!(graph["earliest_known_break"]["node_id"], "step:step-b");
    }

    #[tokio::test]
    async fn incident_filters_and_search_narrow_the_same_projection() {
        let db = incident_fixture().await;
        let ids = |graph: &Value| {
            graph["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n["id"].as_str().unwrap().to_string())
                .collect::<HashSet<_>>()
        };
        // Filter by tool.
        let bash = db
            .incident_view(
                "request".into(),
                IncidentQuery {
                    tool: Some("bash".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        let bash_ids = ids(&bash);
        assert!(bash_ids.contains("step:step-b"));
        assert!(!bash_ids.contains("step:step-a"));
        assert_eq!(bash["query"]["filtered"], json!(true));
        // The unfiltered totals are still reported, so a reviewer sees what was excluded.
        assert!(bash["counts"]["unfiltered"]["nodes"].as_i64().unwrap() > bash_ids.len() as i64);

        // Filter by path: only the mutation carries one.
        let path = db
            .incident_view(
                "request".into(),
                IncidentQuery {
                    path: Some("src/lib".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(ids(&path).contains("mutation:mut-a"));
        assert!(!ids(&path).contains("step:step-a"));

        // Filter by status and by row id.
        let failed = db
            .incident_view(
                "request".into(),
                IncidentQuery {
                    status: Some("failed".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(ids(&failed).contains("step:step-b"));
        let row = db
            .incident_view(
                "request".into(),
                IncidentQuery {
                    row_id: Some("permit-a".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(ids(&row).contains("permission:permit-a"));

        // Free-text search over labels, tokenised by the shared normaliser.
        let text = db
            .incident_view(
                "request".into(),
                IncidentQuery {
                    q: Some("bash".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(ids(&text).contains("step:step-b"));
        assert!(!ids(&text).contains("step:step-a"));

        // Relation filtering removes the edge and therefore its neighborhood.
        let relation = db
            .incident_view(
                "request".into(),
                IncidentQuery {
                    relation: Some("contradicts".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(relation["edges"].as_array().unwrap().is_empty());
        // With no recorded edge left, nothing may still claim a dependency except the frame.
        for node in relation["nodes"].as_array().unwrap() {
            if node["kind"] != json!("request") {
                assert_ne!(node["confidence"], "recorded_dependency");
            }
        }

        // The request frame always survives a filter that matches nothing.
        let empty = db
            .incident_view(
                "request".into(),
                IncidentQuery {
                    tool: Some("no-such-tool".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ids(&empty), HashSet::from(["request:request".to_string()]));

        // Closed graph under every filter: every edge endpoint resolves to a returned node.
        for graph in [&bash, &path, &failed, &row, &text, &relation, &empty] {
            let node_ids = ids(graph);
            for edge in graph["edges"].as_array().unwrap() {
                assert!(node_ids.contains(edge["source"].as_str().unwrap()));
                assert!(node_ids.contains(edge["target"].as_str().unwrap()));
            }
        }
    }

    #[tokio::test]
    async fn incident_timeline_orders_by_recorded_time_and_marks_undated_rows() {
        let db = incident_fixture().await;
        let graph = db
            .incident_view(
                "request".into(),
                IncidentQuery {
                    view: "chronological".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        let timeline = graph["timeline"].as_array().unwrap();
        assert!(!timeline.is_empty());
        assert_eq!(graph["query"]["view"], "chronological");
        // Dated entries come first and are non-decreasing; undated entries are counted, not
        // guessed into an order.
        let mut last: Option<String> = None;
        let mut seen_undated = false;
        for entry in timeline {
            match entry["at"].as_str() {
                Some(at) => {
                    assert!(!seen_undated, "a dated row followed an undated one");
                    if let Some(previous) = &last {
                        assert!(previous.as_str() <= at);
                    }
                    last = Some(at.to_string());
                }
                None => seen_undated = true,
            }
        }
        assert_eq!(
            graph["timeline_undated"].as_i64().unwrap(),
            timeline.iter().filter(|e| e["at"].is_null()).count() as i64
        );
        // The fixture deliberately contains one, so this suite fails if undated rows are ever
        // sorted into the dated sequence rather than listed after it.
        assert!(seen_undated, "the fixture must exercise an undated row");
        assert!(graph["timeline_undated"].as_i64().unwrap() >= 1);
        // Every timeline entry resolves to a node in the same response.
        let node_ids = graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap())
            .collect::<HashSet<_>>();
        for entry in timeline {
            assert!(node_ids.contains(entry["node_id"].as_str().unwrap()));
        }
        // A rejected view is refused rather than silently treated as causal.
        assert!(db
            .incident_view(
                "request".into(),
                IncidentQuery {
                    view: "guessed".into(),
                    ..Default::default()
                }
            )
            .await
            .is_err());
        // A hand-built anchor is refused: cursors are opaque.
        assert!(db
            .incident_view(
                "request".into(),
                IncidentQuery {
                    anchor: Some(json!({"after_node_id": "step:step-a"})),
                    ..Default::default()
                }
            )
            .await
            .is_err());
    }

    /// P16-T02: a second run of the same work, with one deliberate difference (the edit step
    /// succeeds) and one deliberate absence (no bash step at all). That gives the comparison
    /// one `differs` slot and one `missing_evidence` slot to tell apart.
    async fn second_run_fixture(db: &DbStore) {
        db.run(|c| {
            let stamp = now();
            c.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES('request-2','session','user','hi','pending',?1)",[&stamp])?;
            // `failed` rather than `complete`: migration 001's CHECK requires a complete
            // receipt to name an answer row, and inventing one would put a message in this
            // fixture that the comparison does not read.
            c.execute("INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES('request-2','session','global','main','sig',0,'failed',?1,?1)",[&stamp])?;
            c.execute(crate::agentic_sql::STEP_BEGIN,params!["step-c","request-2",None::<String>,0,"tool_call","edit","call-c","{}",stamp])?;
            c.execute(crate::agentic_sql::PERMISSION_CREATE,params!["permit-c","request-2","step-c","edit","edit","{}",stamp,stamp])?;
            c.execute(crate::agentic_sql::PROVENANCE_EDGE_INSERT,params!["edge-c","request-2","step","step-c","depends_on","permission","permit-c",stamp])?;
            c.execute(crate::agentic_sql::STEP_FINISH,params!["step-c","complete","{}",0,0,None::<i64>,None::<i64>,None::<String>,stamp])?;
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn incident_comparison_aligns_by_semantic_identity_and_separates_absence_from_difference()
    {
        let db = incident_fixture().await;
        second_run_fixture(&db).await;
        let report = db
            .compare_incident_runs(vec!["request".into(), "request-2".into()])
            .await
            .unwrap();
        assert_eq!(report["compared"], json!(["request", "request-2"]));
        assert!(report["runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|run| run["found"] == json!(true)));
        let slots = report["aligned"].as_array().unwrap();
        let slot = |name: &str| {
            slots
                .iter()
                .find(|slot| slot["slot"] == json!(name))
                .cloned()
                .unwrap_or_else(|| panic!("missing slot {name}, got {slots:#?}"))
        };
        // The edit step exists in both runs under a different row id, so semantic identity
        // aligned it rather than reporting two unrelated steps.
        let edit = slot("step|edit#0");
        assert_eq!(edit["present_in"], json!(2));
        assert_eq!(edit["verdict"], "differs");
        assert_eq!(edit["missing_in"], json!([]));
        // The bash step is recorded only in the first run. That is an evidence gap, not a
        // difference, and the run it is missing from is named.
        let bash = slot("step|bash#0");
        assert_eq!(bash["verdict"], "missing_evidence");
        assert_eq!(bash["missing_in"], json!(["request-2"]));
        assert_eq!(bash["present_in"], json!(1));
        // The mutation exists only in the first run too.
        assert_eq!(slot("mutation|src/lib.rs#0")["verdict"], "missing_evidence");
        // First divergence is reported once, and it names which class of difference it is.
        let divergence = &report["first_divergence"];
        assert_eq!(divergence["aligned"], json!(true));
        assert!(matches!(
            divergence["difference_kind"].as_str(),
            Some("recorded_difference") | Some("missing_evidence")
        ));
        // Counts add up to the number of slots, so no slot escaped classification.
        let counts = &report["counts"];
        assert_eq!(
            counts["identical"].as_i64().unwrap()
                + counts["differs"].as_i64().unwrap()
                + counts["missing_evidence"].as_i64().unwrap(),
            counts["slots"].as_i64().unwrap()
        );
        assert_eq!(counts["slots"].as_u64().unwrap() as usize, slots.len());
        // The legend defines every verdict the comparator can emit, and says plainly that a
        // missing slot is not proof the step did not happen.
        for verdict in ["identical", "differs", "missing_evidence"] {
            assert!(report["alignment_labels"][verdict].is_string());
        }
        assert!(report["alignment_labels"]["missing_evidence"]
            .as_str()
            .unwrap()
            .contains("not proof"));
        // Never claim a counterfactual.
        let text = report.to_string();
        assert!(!text.contains("would have"), "counterfactual claim leaked");
        no_reasoning_keys(&report);
    }

    #[tokio::test]
    async fn incident_comparison_refuses_bad_input_and_excludes_runs_with_no_evidence() {
        let db = incident_fixture().await;
        // Fewer than two distinct runs is refused rather than answered with a trivial report.
        assert!(db
            .compare_incident_runs(vec!["request".into()])
            .await
            .is_err());
        assert!(db
            .compare_incident_runs(vec!["request".into(), "request".into()])
            .await
            .is_err());
        assert!(db
            .compare_incident_runs(
                (0..MAX_COMPARED_RUNS + 1)
                    .map(|i| format!("r{i}"))
                    .collect()
            )
            .await
            .is_err());
        // A run with no recording receipt contributes no evidence: it is reported as not
        // found and excluded, instead of being treated as an empty graph that would make
        // every slot look like a difference.
        let report = db
            .compare_incident_runs(vec!["request".into(), "missing-run".into()])
            .await
            .unwrap();
        let absent = report["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["request_id"] == json!("missing-run"))
            .unwrap()
            .clone();
        assert_eq!(absent["found"], json!(false));
        assert_eq!(report["aligned"].as_array().unwrap().len(), 0);
        assert_eq!(report["first_divergence"]["aligned"], json!(false));
        assert_eq!(report["counts"]["differs"], json!(0));
    }

    #[tokio::test]
    async fn incident_export_is_sanitized_checksummed_and_labels_its_evidence() {
        let db = incident_fixture().await;
        let artifact = db
            .export_incident_graph("request".into(), "json".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(artifact["format"], "causal-graph-export-v1");
        assert_eq!(artifact["export_format"], "json");
        // The exported graph is the P16-T01 projection, not a second query: the node set and
        // the counts are the ones the incident endpoint returns.
        let graph = db.incident_graph("request".into()).await.unwrap().unwrap();
        assert_eq!(artifact["counts"], graph["counts"]);
        assert_eq!(artifact["bounds"], graph["bounds"]);
        assert_eq!(
            artifact["earliest_known_break"],
            graph["earliest_known_break"]
        );
        assert_eq!(
            artifact["nodes"].as_array().unwrap().len(),
            graph["nodes"].as_array().unwrap().len()
        );
        // Every label that shipped is exactly what the shared sanitizer would emit, so an
        // export can never be more permissive than the search index.
        for node in artifact["nodes"].as_array().unwrap() {
            if node["label_sanitized"] == json!(true) {
                assert!(crate::export::review::is_sanitized(
                    node["label"].as_str().unwrap()
                ));
            }
        }
        // Evidence labels are the four documented ones and nothing else, and each has a legend.
        for node in artifact["nodes"].as_array().unwrap() {
            let evidence = node["evidence"].as_str().unwrap();
            assert!(
                artifact["evidence_labels"][evidence].is_string(),
                "{evidence}"
            );
        }
        for edge in artifact["edges"].as_array().unwrap() {
            let evidence = edge["evidence"].as_str().unwrap();
            assert!(
                artifact["evidence_labels"][evidence].is_string(),
                "{evidence}"
            );
        }
        // A recorded contradiction/invalidation is a distinct class from a dependency: the
        // design's success criteria require telling contradictory evidence from missing.
        assert_eq!(
            crate::storage::incident_compare::evidence_label(Some("contradicts"), None),
            "contradiction"
        );
        // The checksum is over the artifact with the checksum blanked, using the same shared
        // canonical serialization continuation packets use, so it is verifiable the same way.
        let mut copy = artifact.clone();
        let claimed = copy["content_sha256"].as_str().unwrap().to_string();
        // The digest is taken over the artifact as it stood before the checksum, the export
        // format and the citation digest were stamped into it, so verification removes
        // exactly those and nothing else.
        let object = copy.as_object_mut().unwrap();
        object.remove("content_sha256");
        object.remove("export_format");
        if let Some(citation) = copy["citation"].as_object_mut() {
            citation.insert("content_sha256".into(), json!(""));
        }
        assert_eq!(
            claimed,
            crate::export::review::checksum(&crate::export::packet::canonical_json(&copy))
        );
        assert_eq!(artifact["citation"]["content_sha256"], json!(claimed));
        assert_eq!(
            artifact["citation"]["sanitizer"],
            crate::export::review::SANITIZER
        );
        no_reasoning_keys(&artifact);
        // Graphviz is a second rendering of the same sanitized artifact, never a second read.
        let dot = db
            .export_incident_graph("request".into(), "graphviz".into())
            .await
            .unwrap()
            .unwrap();
        let text = dot["graphviz"].as_str().unwrap();
        assert!(text.starts_with("digraph causal_graph {"));
        for node in dot["nodes"].as_array().unwrap() {
            assert!(text.contains(node["id"].as_str().unwrap()));
        }
        assert!(text.contains("not causation"));
        no_reasoning_keys(&dot);
        // An unsupported format is refused rather than silently defaulted, and an unknown
        // request has no artifact at all.
        assert!(db
            .export_incident_graph("request".into(), "svg".into())
            .await
            .is_err());
        assert!(db
            .export_incident_graph("missing".into(), "json".into())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn incident_coverage_counts_missing_edges_without_claiming_they_had_no_cause() {
        let db = incident_fixture().await;
        let report = db.causal_coverage("request".into()).await.unwrap().unwrap();
        assert_eq!(report["format"], "causal-coverage-v1");
        // Coverage is measured over the same projection a reviewer opens, so the graph size it
        // reports is the projection's own count rather than a second tally.
        let graph = db.incident_graph("request".into()).await.unwrap().unwrap();
        assert_eq!(report["graph_size"]["nodes"], graph["counts"]["nodes"]);
        assert_eq!(report["graph_size"]["edges"], graph["counts"]["edges"]);
        // Only a recorded edge counts toward coverage: proximity must not inflate it, or the
        // metric would report a level of recorded provenance that does not exist.
        let breakdown = &report["evidence_breakdown"];
        let nodes = graph["nodes"].as_array().unwrap();
        let recorded = nodes
            .iter()
            .filter(|n| n["confidence"] == json!("recorded_dependency"))
            .count() as i64;
        let proximity = nodes
            .iter()
            .filter(|n| n["confidence"] == json!("temporal_proximity"))
            .count() as i64;
        assert_eq!(breakdown["recorded_dependency"], json!(recorded));
        assert_eq!(breakdown["temporal_proximity"], json!(proximity));
        assert!(proximity > 0, "fixture must exercise a proximity-only row");
        assert_eq!(
            report["missing_edges"],
            json!(nodes.len() as i64 - recorded)
        );
        // The three classes partition the node set: nothing escaped measurement.
        assert_eq!(
            breakdown["recorded_dependency"].as_i64().unwrap()
                + breakdown["temporal_proximity"].as_i64().unwrap()
                + breakdown["unknown"].as_i64().unwrap(),
            nodes.len() as i64
        );
        // A missing edge is described as an evidence gap, never as a row proven causeless.
        assert!(report["metric_labels"]["missing_edges"]
            .as_str()
            .unwrap()
            .contains("not a claim"));
        // The fixture has a recorded break, and it is the same node the projection names.
        assert_eq!(report["earliest_break"]["recorded"], json!(true));
        assert_eq!(
            report["earliest_break"]["node_id"],
            graph["earliest_known_break"]["node_id"]
        );
        // Reviewer time is null-not-zero before anything has been measured.
        assert_eq!(report["reviewer_time"]["measured"], json!(false));
        assert_eq!(report["reviewer_time"]["mean_ms"], Value::Null);
        no_reasoning_keys(&report);
        assert!(db.causal_coverage("nope".into()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn coverage_with_no_recorded_break_says_unknown_rather_than_success() {
        let db = DbStore::init(":memory:").unwrap();
        db.run(|c| {
            let stamp = now();
            c.execute("INSERT INTO sessions(id,scope,created_at) VALUES('s','global',?1)",[&stamp])?;
            c.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES('r','s','user','hi','pending',?1)",[&stamp])?;
            c.execute("INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES('r','s','global','m','sig',0,'generating',?1,?1)",[&stamp])?;
            Ok(())
        }).await.unwrap();
        let report = db.causal_coverage("r".into()).await.unwrap().unwrap();
        assert_eq!(report["earliest_break"]["recorded"], json!(false));
        assert_eq!(report["earliest_break"]["kind"], "unknown");
        // The wording must not let an absence read as a success.
        let reason = report["earliest_break"]["reason"].as_str().unwrap();
        assert!(reason.contains("absence of evidence"));
        assert!(reason.contains("not evidence the run succeeded"));
    }

    #[tokio::test]
    async fn reviewer_time_is_measured_and_never_assumed() {
        let db = incident_fixture().await;
        let open = db
            .open_incident_review("request".into(), "causal".into(), 6, 2)
            .await
            .unwrap();
        // An open review contributes no duration: it is counted, not estimated.
        let during = db.reviewer_time("request".into()).await.unwrap();
        assert_eq!(during["open_reviews"], json!(1));
        assert_eq!(during["closed_reviews"], json!(0));
        assert_eq!(during["mean_ms"], Value::Null);
        assert_eq!(during["measured"], json!(false));
        assert!(db
            .close_incident_review(open.clone(), 2500, "cause_identified".into())
            .await
            .unwrap());
        // Closing twice is refused rather than silently re-measuring the same session.
        assert!(!db
            .close_incident_review(open, 999, "unknown".into())
            .await
            .unwrap());
        let after = db.reviewer_time("request".into()).await.unwrap();
        assert_eq!(after["closed_reviews"], json!(1));
        assert_eq!(after["open_reviews"], json!(0));
        assert_eq!(after["mean_ms"], json!(2500));
        assert_eq!(after["max_ms"], json!(2500));
        assert_eq!(after["cause_identified_reviews"], json!(1));
        assert_eq!(after["measured"], json!(true));
        // An abandoned review is a real recorded outcome, counted separately from a solve.
        let abandoned = db
            .open_incident_review("request".into(), "comparison".into(), 6, 2)
            .await
            .unwrap();
        assert!(db
            .close_incident_review(abandoned, 100, "abandoned".into())
            .await
            .unwrap());
        let final_time = db.reviewer_time("request".into()).await.unwrap();
        assert_eq!(final_time["abandoned_reviews"], json!(1));
        assert_eq!(final_time["cause_identified_reviews"], json!(1));
        // Unsupported views, outcomes and negative durations are refused, not coerced.
        assert!(db
            .open_incident_review("request".into(), "guessed".into(), 1, 1)
            .await
            .is_err());
        assert!(db
            .close_incident_review("x".into(), 1, "solved".into())
            .await
            .is_err());
        assert!(db
            .close_incident_review("x".into(), -1, "unknown".into())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn deployment_provenance_records_a_causal_trail_and_flags_anomalies() {
        let db = DbStore::init(":memory:").unwrap();
        let build = db
            .record_deployment_event(
                "deploy-1".into(),
                None,
                "build".into(),
                "started".into(),
                Some("abc1234".into()),
                Some("f".repeat(64)),
                Some(15),
                Some("cargo build --locked --release".into()),
            )
            .await
            .unwrap();
        // A phase's result is the one thing that may be filled in later.
        assert!(db
            .finish_deployment_event(
                build.clone(),
                "succeeded".into(),
                None,
                DeploymentAnomalies::default(),
            )
            .await
            .unwrap());
        // Finishing twice is refused: recorded provenance is not rewritable.
        assert!(!db
            .finish_deployment_event(
                build.clone(),
                "failed".into(),
                None,
                DeploymentAnomalies::default(),
            )
            .await
            .unwrap());
        // The restart phase records which build it actually followed, rather than being
        // inferred from whichever row happens to be adjacent.
        let restart = db
            .record_deployment_event(
                "deploy-1".into(),
                Some(build.clone()),
                "restart".into(),
                "succeeded".into(),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let smoke = db
            .record_deployment_event(
                "deploy-1".into(),
                Some(restart.clone()),
                "smoke".into(),
                "started".into(),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        assert!(db
            .finish_deployment_event(
                smoke,
                "failed".into(),
                Some("authenticated API smoke failed".into()),
                DeploymentAnomalies {
                    smoke_failed: Some(true),
                    unready: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap());
        let trail = db
            .deployment_provenance(Some("deploy-1".into()))
            .await
            .unwrap();
        let events = trail["events"].as_array().unwrap();
        assert_eq!(events.len(), 3);
        // The trail is a recorded chain, not a guessed one.
        assert_eq!(events[0]["parent_id"], Value::Null);
        assert_eq!(events[1]["parent_id"], json!(build));
        assert_eq!(events[2]["parent_id"], json!(restart));
        assert_eq!(events[0]["schema_version"], json!(15));
        // An anomaly that was answered `false` is distinct from one never asked, which is null.
        let flags = &events[2]["anomalies"];
        assert_eq!(flags["smoke_failed"], json!(true));
        assert_eq!(flags["unready"], json!(false));
        assert_eq!(flags["identity_mismatch"], Value::Null);
        assert_eq!(flags["schema_regressed"], Value::Null);
        // The flagged phase is surfaced so an operator does not have to scan the trail.
        assert_eq!(trail["counts"]["anomalies"], json!(1));
        assert_eq!(trail["anomaly_flags"][0]["phase"], "smoke");
        assert!(trail["note"].as_str().unwrap().contains("never inferred"));
        no_reasoning_keys(&trail);
        // Unsupported phases and statuses are refused rather than coerced into the vocabulary.
        assert!(db
            .record_deployment_event(
                "deploy-1".into(),
                None,
                "guessed".into(),
                "started".into(),
                None,
                None,
                None,
                None
            )
            .await
            .is_err());
        assert!(db
            .record_deployment_event(
                "deploy-1".into(),
                None,
                "build".into(),
                "probably".into(),
                None,
                None,
                None,
                None
            )
            .await
            .is_err());
        // A phase whose result was never observed is recordable as `unknown`, never as a
        // success or a failure.
        let unknown = db
            .record_deployment_event(
                "deploy-2".into(),
                None,
                "outcome".into(),
                "unknown".into(),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        assert!(!unknown.is_empty());
        let recent = db.deployment_provenance(None).await.unwrap();
        assert_eq!(recent["counts"]["events"], json!(4));
        assert_eq!(recent["counts"]["unresolved_or_unknown"], json!(1));
    }

    #[tokio::test]
    async fn coverage_summary_excludes_unrecorded_requests_instead_of_averaging_them_in() {
        let db = incident_fixture().await;
        assert!(db.causal_coverage_summary(vec![]).await.is_err());
        assert!(db
            .causal_coverage_summary(
                (0..MAX_COVERAGE_REQUESTS + 1)
                    .map(|i| format!("r{i}"))
                    .collect()
            )
            .await
            .is_err());
        let summary = db
            .causal_coverage_summary(vec!["request".into(), "not-recorded".into()])
            .await
            .unwrap();
        assert_eq!(summary["counts"]["requested"], json!(2));
        assert_eq!(summary["counts"]["measured"], json!(1));
        assert_eq!(summary["counts"]["not_recorded"], json!(1));
        // Per-request figures survive beside the aggregate, so an audit bundle can cite the
        // individual measurement rather than only a rolled-up number.
        let measured = summary["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["request_id"] == json!("request"))
            .unwrap()
            .clone();
        assert_eq!(measured["format"], "causal-coverage-v1");
        assert!(
            measured["graph_size"]["nodes"]["returned"]
                .as_i64()
                .unwrap()
                > 0
        );
        // The aggregate is over measured requests only, and says so.
        assert_eq!(
            summary["aggregate"]["nodes"],
            measured["graph_size"]["nodes"]["returned"]
        );
        assert!(summary["note"].as_str().unwrap().contains("not measured"));
        no_reasoning_keys(&summary);
    }

    /// The P16-T01 no-chain-of-thought contract, applied recursively to every new response
    /// P16-T03: run the REAL coverage path over the declared fixtures and write the metrics the
    /// `tests/coverage_eval/run.py --check` gate validates.
    ///
    /// The measurement lives here rather than in Python for the reason P15-T01 recorded for the
    /// recall gate: a Python reimplementation would measure the reimplementation. This produces
    /// the evidence; the script refuses to accept evidence that is missing, stale relative to
    /// the fixtures, internally inconsistent, or outside a declared budget.
    #[tokio::test]
    async fn causal_coverage_metrics_meet_declared_budgets() {
        use std::io::Write;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/coverage_eval");
        let fixture_bytes = std::fs::read(root.join("fixtures.json")).unwrap();
        let fixtures: Value = serde_json::from_slice(&fixture_bytes).unwrap();
        let fixture_sha256 = crate::safety::fingerprint(&String::from_utf8_lossy(&fixture_bytes));

        // Case 1 is the P16-T01 fixture: recorded edges plus a genuinely edge-less row.
        let db = incident_fixture().await;
        // Case 2 is a turn with nothing broken recorded at all.
        db.run(|c| {
            let stamp = now();
            c.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES('quiet','session','user','hi','pending',?1)",[&stamp])?;
            c.execute("INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES('quiet','session','global','main','sig',0,'generating',?1,?1)",[&stamp])?;
            Ok(())
        }).await.unwrap();

        let mut cases = Vec::<Value>::new();
        for (id, request) in [
            ("stale-anchor-with-recorded-edges", "request"),
            ("no-recorded-break", "quiet"),
        ] {
            let report = db
                .causal_coverage(request.into())
                .await
                .unwrap()
                .expect("fixture request must be recorded");
            cases.push(json!({
                "id": id,
                "request_id": request,
                "nodes": report["graph_size"]["nodes"]["returned"],
                "edges": report["graph_size"]["edges"]["returned"],
                "recorded_dependency": report["evidence_breakdown"]["recorded_dependency"],
                "temporal_proximity": report["evidence_breakdown"]["temporal_proximity"],
                "unknown": report["evidence_breakdown"]["unknown"],
                "missing_edges": report["missing_edges"],
                "edge_coverage": report["edge_coverage"],
                "break_recorded": report["earliest_break"]["recorded"],
                "break_kind": report["earliest_break"]["kind"],
            }));
        }

        // Reviewer time has to be measured from a real opened-and-closed session, not asserted.
        let review = db
            .open_incident_review("request".into(), "causal".into(), 7, 2)
            .await
            .unwrap();
        assert!(db
            .close_incident_review(review, 1800, "cause_identified".into())
            .await
            .unwrap());
        let reviewer_time = db.reviewer_time("request".into()).await.unwrap();

        // A recorded deployment trail, built the way scripts/deploy.sh builds one.
        let build = db
            .record_deployment_event(
                "deploy-fixture".into(),
                None,
                "build".into(),
                "started".into(),
                Some("abc1234".into()),
                Some("a".repeat(64)),
                Some(15),
                None,
            )
            .await
            .unwrap();
        assert!(db
            .finish_deployment_event(
                build.clone(),
                "succeeded".into(),
                None,
                DeploymentAnomalies {
                    identity_mismatch: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap());
        let restart = db
            .record_deployment_event(
                "deploy-fixture".into(),
                Some(build),
                "restart".into(),
                "succeeded".into(),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        db.record_deployment_event(
            "deploy-fixture".into(),
            Some(restart),
            "smoke".into(),
            "succeeded".into(),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let trail = db
            .deployment_provenance(Some("deploy-fixture".into()))
            .await
            .unwrap();
        let phases: Vec<Value> = trail["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| {
                json!({
                    "id": event["id"],
                    "parent_id": event["parent_id"],
                    "phase": event["phase"],
                    "status": event["status"],
                    "anomalies": event["anomalies"],
                })
            })
            .collect();

        let metrics = json!({
            "fixture_sha256": fixture_sha256,
            "projection": provenance_projection(),
            "cases": cases,
            "reviewer_time": {
                "closed_reviews": reviewer_time["closed_reviews"],
                "open_reviews": reviewer_time["open_reviews"],
                "abandoned_reviews": reviewer_time["abandoned_reviews"],
                "mean_ms": reviewer_time["mean_ms"],
                "max_ms": reviewer_time["max_ms"],
            },
            "deployment": {
                "phases": phases,
                "anomalies_flagged": trail["counts"]["anomalies"],
                "unresolved_or_unknown": trail["counts"]["unresolved_or_unknown"],
            },
        });
        let mut file = std::fs::File::create(root.join("metrics.json")).unwrap();
        writeln!(file, "{}", serde_json::to_string_pretty(&metrics).unwrap()).unwrap();

        // Assert the fixture's own declarations here too, so the Rust test fails on a real
        // regression even if someone runs it without the Python gate.
        for declared in fixtures["cases"].as_array().unwrap() {
            let id = declared["id"].as_str().unwrap();
            let got = cases
                .iter()
                .find(|case| case["id"] == json!(id))
                .unwrap_or_else(|| panic!("case {id} was not measured"));
            assert_eq!(
                got["break_recorded"], declared["expect_break_recorded"],
                "case {id} break"
            );
            assert_eq!(
                got["recorded_dependency"], declared["expect_recorded_dependency"],
                "case {id} recorded dependencies"
            );
            assert_eq!(
                got["temporal_proximity"], declared["expect_temporal_proximity"],
                "case {id} proximity"
            );
            // The partition invariant the gate checks, asserted at the source of the numbers.
            assert_eq!(
                got["recorded_dependency"].as_i64().unwrap()
                    + got["temporal_proximity"].as_i64().unwrap()
                    + got["unknown"].as_i64().unwrap(),
                got["nodes"].as_i64().unwrap(),
                "case {id} evidence classes must partition its nodes"
            );
        }
    }

    /// shape rather than only to the incident graph it was written for.
    fn no_reasoning_keys(value: &Value) {
        fn walk(value: &Value, out: &mut Vec<String>) {
            match value {
                Value::Object(map) => {
                    for (key, child) in map {
                        out.push(key.clone());
                        walk(child, out);
                    }
                }
                Value::Array(items) => items.iter().for_each(|item| walk(item, out)),
                _ => {}
            }
        }
        let mut names = Vec::new();
        walk(value, &mut names);
        for name in &names {
            for banned in ["thought", "reasoning", "rationale", "explanation"] {
                assert!(!name.contains(banned), "leaked key {name}");
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
            c.execute(
                "INSERT INTO sessions(id,scope,created_at) VALUES('s','global','now')",
                [],
            )?;
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
            background_requests_per_day: None,
            foreground_reserve_per_day: 1,
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

    /// Fairness: background roles must leave daily headroom for the request a user is waiting
    /// on, and may carry a ceiling of their own. Both decisions have to be durable refusals
    /// recorded in the ledger, exactly like the P14-T04a limits they extend.
    #[tokio::test]
    async fn background_provider_roles_yield_daily_headroom_to_foreground_work() {
        fn limits(background_per_day: Option<u64>, per_day: u64) -> SpendLimits {
            SpendLimits {
                requests_per_turn: 100,
                requests_per_day: per_day,
                background_requests_per_day: background_per_day,
                foreground_reserve_per_day: 1,
                tokens_per_turn: None,
                tokens_per_day: None,
                cost_microusd_per_turn: None,
                cost_microusd_per_day: None,
                input_microusd_per_million: None,
                output_microusd_per_million: None,
            }
        }

        // Daily budget of 3 with 1 reserved: background may take 2, then must yield.
        let db = DbStore::init(":memory:").unwrap();
        for _ in 0..2 {
            db.reserve_provider_call(
                Some("request".into()),
                "compaction".into(),
                "model".into(),
                limits(None, 3),
            )
            .await
            .expect("background work is allowed while headroom remains");
        }
        let refused = db
            .reserve_provider_call(
                Some("request".into()),
                "extraction".into(),
                "model".into(),
                limits(None, 3),
            )
            .await
            .unwrap_err();
        assert!(
            refused.to_string().contains("foreground_reserve"),
            "background work must yield the reserved slot, got {refused}"
        );
        // The slot it yielded is still available to the user's own turn.
        db.reserve_provider_call(
            Some("request".into()),
            "model_call".into(),
            "model".into(),
            limits(None, 3),
        )
        .await
        .expect("foreground work may spend the reserved slot");
        let logged: i64 = db
            .run(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM provider_calls WHERE state='refused' AND reason='foreground_reserve'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(logged, 1, "the refusal must be durable");

        // A background-only ceiling binds even when the shared budget is wide open.
        let db = DbStore::init(":memory:").unwrap();
        db.reserve_provider_call(
            Some("request".into()),
            "verification".into(),
            "model".into(),
            limits(Some(1), 500),
        )
        .await
        .unwrap();
        let capped = db
            .reserve_provider_call(
                Some("request".into()),
                "verification".into(),
                "model".into(),
                limits(Some(1), 500),
            )
            .await
            .unwrap_err();
        assert!(
            capped.to_string().contains("background_request_limit"),
            "the background ceiling must bind independently, got {capped}"
        );
        db.reserve_provider_call(
            Some("request".into()),
            "model_call".into(),
            "model".into(),
            limits(Some(1), 500),
        )
        .await
        .expect("a background ceiling must never block foreground work");
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
