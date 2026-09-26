//! Recording receipts: durable admission, a serial generation worker, and a memory outbox.
//! Sanitized text only. This is NOT an encrypted exact-original archive.
use crate::{
    agent_loop, agentic_sql as agentic, context,
    ingest::Event,
    memory_agents::{BoxFuture, BufferedGeneration, GenerationSink, MemoryAgents, ModelUsage},
    providers::ProviderRegistry,
    recording_sql as sql, safety,
    storage::{now, uid, DbStore, Recall, ScopeConfig},
    tools::Registry,
};
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Instant;

pub struct CaptureInput {
    pub request: String,
    pub session: String,
    pub scope: String,
    pub prompt: String,
    pub model: String,
    pub signature: String,
    pub redacted: bool,
}
pub enum Admission {
    Saved(Value),
    Conflict,
    ScopeConflict,
    Busy,
    Full,
}
pub enum RetryAdmission {
    Saved(Value),
    Unsafe,
    NotTerminal,
    Busy,
    NotFound,
}

/// The states a chat receipt can be in.
///
/// The strings are a durable contract, not an implementation detail: they are
/// stored in `chat_receipts.state`, filtered on by the admission queries, and
/// echoed verbatim in API responses. This enum exists so the pending/terminal
/// rule is written once instead of being re-derived by string comparison at
/// every site that needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestState {
    Captured,
    Generating,
    Complete,
    Failed,
    Interrupted,
}

impl RequestState {
    /// `None` for a state this build does not know. Callers decide what an
    /// unrecognised state means; it is never assumed to be terminal.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "captured" => Some(Self::Captured),
            "generating" => Some(Self::Generating),
            "complete" => Some(Self::Complete),
            "failed" => Some(Self::Failed),
            "interrupted" => Some(Self::Interrupted),
            _ => None,
        }
    }

    /// An answer is still owed for this request. Mirrors the SQL predicate
    /// `state IN ('captured','generating')` used by the admission queries.
    pub fn is_pending(self) -> bool {
        matches!(self, Self::Captured | Self::Generating)
    }
}

/// Publishes generation text to the durable event feed as it arrives.
///
/// Contract (docs/design/incremental-publication.md): the text handed to `delta` is already
/// redacted by `safety::StreamRedactor`, and each chunk row is committed *before* delivery, so a
/// reader can never observe a chunk that was not durable first. Publication failures are
/// recorded on the sink and reported once, when the turn ends, instead of being buried.
pub struct RecordingGenerationSink<'a> {
    store: &'a DbStore,
    request: String,
    session: String,
    failure: Option<&'static str>,
    buffered: BufferedGeneration,
    observer: crate::runtime_observability::RuntimeObserver,
}

impl<'a> RecordingGenerationSink<'a> {
    pub fn new(
        store: &'a DbStore,
        request: String,
        session: String,
        observer: crate::runtime_observability::RuntimeObserver,
    ) -> Self {
        Self {
            store,
            request,
            session,
            failure: None,
            buffered: BufferedGeneration::default(),
            observer,
        }
    }

    /// The first publication failure, if any. `generate` turns this into `fail_recording`.
    pub fn failure(&self) -> Option<&'static str> {
        self.failure
    }

    async fn publish(&mut self, text: &str) {
        if self.failure.is_some() {
            return;
        }
        let started = Instant::now();
        let failed = self
            .store
            .append_generation(
                self.request.clone(),
                self.session.clone(),
                "chunk".to_string(),
                text.to_string(),
                None,
            )
            .await
            .is_err();
        self.observer.publication(started.elapsed());
        if failed {
            self.failure = Some("generation_stream_save_failed");
        }
    }
}

impl GenerationSink for RecordingGenerationSink<'_> {
    fn delta<'a>(&'a mut self, text: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.buffered.delta(text).await;
            self.publish(text).await;
        })
    }

    fn complete<'a>(&'a mut self, usage: &'a ModelUsage) -> BoxFuture<'a, ()> {
        // The terminal row is written with the receipt, never here.
        Box::pin(async move { self.buffered.complete(usage).await })
    }

    fn fail<'a>(&'a mut self, _error_code: &'a str) -> BoxFuture<'a, ()> {
        // Failure rows are owned by `fail_recording`.
        Box::pin(async move {})
    }
    fn text(&self) -> &str {
        self.buffered.text()
    }
    fn usage(&self) -> Option<&ModelUsage> {
        self.buffered.usage()
    }
}
pub struct Generation {
    pub request: String,
    pub session: String,
    pub scope: String,
    pub model: String,
    pub provider_id: String,
    pub provider_version: u64,
    pub prompt: String,
    pub events: Vec<Event>,
}

type RetrySource = (String, String, String, String, String, bool, String, u64);
type ClaimedRecording = (String, String, String, String, String, u64, String, i64);

pub fn recover(c: &mut Connection) -> Result<()> {
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stamp = now();
    // Committed Stop intent wins over generic restart recovery in this same transaction.
    let cancelled = {
        let mut stmt = tx.prepare(
            "SELECT r.request_id,r.session_id FROM chat_receipts r JOIN run_controls c ON c.request_id=r.request_id WHERE r.state='generating' AND c.cancel_requested_at IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (request, session) in cancelled {
        cancel_tx(&tx, &request, &session, &stamp)?;
    }
    tx.execute(sql::RECOVER_EVENTS, [&stamp])?;
    // Agentic rows first: both `RECOVER_ACTIVITY` and `RECOVER_EVENTS` select the receipts that
    // are still `generating`, so they have to run before the receipt itself is interrupted.
    // A `running` step becomes `interrupted`, a pending approval expires, and nothing is
    // retried: `claim_recording` only ever claims a `captured` receipt, so no tool and no
    // possibly billed provider call is repeated after a restart.
    tx.execute(agentic::RECOVER_STEPS, [&stamp])?;
    tx.execute(agentic::RECOVER_PERMISSIONS, [&stamp])?;
    tx.execute(agentic::RECOVER_ACTIVITY, [&stamp])?;
    tx.execute(agentic::RECOVER_PROVENANCE, [&stamp])?;
    // Select only receipts transitioning in this transaction. Historical interruptions
    // must not acquire another generation event (and another replay cursor) on startup.
    tx.execute(
        "INSERT INTO generation_events(request_id,session_id,state,content,error_code,created_at) SELECT request_id,session_id,'interrupted','', 'process_restarted', ?1 FROM chat_receipts WHERE state='generating'",
        [&stamp],
    )?;
    tx.execute(sql::RECOVER, [&stamp])?;
    tx.execute(sql::RECOVER_MESSAGES, [])?;
    tx.execute(
        "UPDATE jobs SET status='pending' WHERE status='running'",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

fn cancel_tx(
    tx: &rusqlite::Transaction<'_>,
    request: &str,
    session: &str,
    stamp: &str,
) -> Result<()> {
    tx.execute(
        "UPDATE turn_steps SET status='interrupted',error_code='cancelled',finished_at=?2 WHERE request_id=?1 AND status='running'",
        params![request, stamp],
    )?;
    tx.execute(
        "UPDATE permission_requests SET status='expired',resolved_at=?2 WHERE request_id=?1 AND status='pending'",
        params![request, stamp],
    )?;
    tx.execute(
        "UPDATE chat_receipts SET state='interrupted',error_code='cancelled',updated_at=?2 WHERE request_id=?1 AND state IN ('captured','generating')",
        params![request, stamp],
    )?;
    tx.execute(
        "UPDATE messages SET status='failed' WHERE id=?1 AND status='pending'",
        [request],
    )?;
    tx.execute(
        "UPDATE run_controls SET cancelled_at=COALESCE(cancelled_at,?2) WHERE request_id=?1",
        params![request, stamp],
    )?;
    tx.execute(sql::EVENT, params![request, "interrupted", stamp])?;
    tx.execute(
        agentic::EVENT,
        params![
            request,
            session,
            None::<String>,
            "turn_cancelled",
            "{}",
            stamp
        ],
    )?;
    tx.execute(
        "INSERT INTO generation_events(request_id,session_id,state,content,error_code,created_at) VALUES(?1,?2,'interrupted','','cancelled',?3)",
        params![request, session, stamp],
    )?;
    Ok(())
}

// The cancellation check must share the terminal transaction, not just precede its await.
fn cancel_pending_tx(tx: &rusqlite::Transaction<'_>, request: &str, stamp: &str) -> Result<bool> {
    let session: Option<String> = tx.query_row(
        "SELECT r.session_id FROM chat_receipts r JOIN run_controls c ON c.request_id=r.request_id WHERE r.request_id=?1 AND r.state='generating' AND c.cancel_requested_at IS NOT NULL",
        [request],
        |r| r.get(0),
    ).optional()?;
    let Some(session) = session else {
        return Ok(false);
    };
    cancel_tx(tx, request, &session, stamp)?;
    Ok(true)
}

fn receipt(c: &Connection, request: &str) -> Result<Option<Value>> {
    let row = c.query_row(
        "SELECT r.request_id,r.session_id,r.scope,r.model,r.provider_id,r.provider_version,r.redacted,r.state,r.error_code,r.captured_at,r.updated_at,r.context_json,a.content,o.job_id,j.status,c.cancel_requested_at,c.cancelled_at,c.safe_boundary_seq,c.retry_of,c.retried_by FROM chat_receipts r LEFT JOIN messages a ON a.id=r.answer_id LEFT JOIN recording_outbox o ON o.request_id=r.request_id LEFT JOIN jobs j ON j.id=o.job_id LEFT JOIN run_controls c ON c.request_id=r.request_id WHERE r.request_id=?1",
        [request], |r| {
            let state: String = r.get(7)?;
            let context: Option<String> = r.get(11)?;
            let context: Value = context.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or(Value::Null);
            let job_status: Option<String> = r.get(14)?;
            let memory_status = job_status.unwrap_or_else(|| if RequestState::parse(&state).is_some_and(RequestState::is_pending) {"waiting_for_turn".into()} else {"deferred".into()});
            Ok(json!({"request_id":r.get::<_,String>(0)?,"session_id":r.get::<_,String>(1)?,
                "scope":r.get::<_,String>(2)?,"model":r.get::<_,String>(3)?,
                "provider_id":r.get::<_,String>(4)?,"provider_version":r.get::<_,i64>(5)?,
                "redacted":r.get::<_,bool>(6)?,
                "state":state,"error_code":r.get::<_,Option<String>>(8)?,"captured_at":r.get::<_,String>(9)?,
                "updated_at":r.get::<_,String>(10)?,"response":r.get::<_,Option<String>>(12)?,
                "memory_job_id":r.get::<_,Option<String>>(13)?,"memory_status":memory_status,
                "recording":"sanitized_local","context_available":!context.is_null(),
                "recalled":context.get("memories").cloned().unwrap_or(json!([])),
                "recalled_context_applied":context.get("memories").and_then(Value::as_array).is_some_and(|m|!m.is_empty()),
                "confirmation_prompt":null,
                "cancel_requested_at":r.get::<_,Option<String>>(15)?,"cancelled_at":r.get::<_,Option<String>>(16)?,
                "safe_boundary_seq":r.get::<_,Option<i64>>(17)?,"retry_of":r.get::<_,Option<String>>(18)?,
                "retried_by":r.get::<_,Option<String>>(19)?}))
        }).optional()?;
    Ok(row)
}

impl DbStore {
    #[cfg(test)]
    pub async fn capture_chat(&self, input: CaptureInput) -> Result<Admission> {
        self.capture_chat_with_provider(input, "environment".into(), 1)
            .await
    }
    pub async fn capture_chat_with_provider(
        &self,
        input: CaptureInput,
        provider_id: String,
        provider_version: u64,
    ) -> Result<Admission> {
        if provider_id.is_empty() || provider_id.len() > 64 || provider_version == 0 {
            bail!("invalid provider routing metadata");
        }
        self.run(move |c| {
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let prior: Option<String> = tx.query_row("SELECT signature FROM chat_receipts WHERE request_id=?1", [&input.request], |r|r.get(0)).optional()?;
            if let Some(signature)=prior {
                return if signature==input.signature {Ok(Admission::Saved(receipt(&tx,&input.request)?.ok_or_else(||anyhow::anyhow!("chat receipt {} has a signature row but no receipt row",input.request))?))} else {Ok(Admission::Conflict)};
            }
            // Old message identifiers cannot be reused either; no invented legacy receipt.
            if tx.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1)",[&input.request],|r|r.get::<_,bool>(0))? {return Ok(Admission::Conflict);}
            let pending:i64=tx.query_row("SELECT count(*) FROM chat_receipts WHERE state IN ('captured','generating')",[],|r|r.get(0))?;
            if pending>=100 {return Ok(Admission::Full);}
            tx.execute("INSERT INTO sessions(id,scope,created_at) VALUES(?1,?2,?3) ON CONFLICT(id) DO NOTHING",params![input.session,input.scope,now()])?;
            let actual:String=tx.query_row("SELECT scope FROM sessions WHERE id=?1",[&input.session],|r|r.get(0))?;
            if actual!=input.scope {return Ok(Admission::ScopeConflict);}
            if tx.query_row("SELECT EXISTS(SELECT 1 FROM chat_receipts WHERE session_id=?1 AND state IN ('captured','generating'))",[&input.session],|r|r.get::<_,bool>(0))? {return Ok(Admission::Busy);}
            let stamp=now();
            tx.execute(sql::INSERT_MESSAGE,params![input.request,input.session,input.prompt,stamp])?;
            tx.execute(sql::INSERT_RECEIPT,params![input.request,input.session,input.scope,input.model,input.signature,input.redacted,stamp,provider_id,provider_version])?;
            tx.execute(sql::INSERT_OUTBOX,params![input.request,stamp])?;
            tx.execute(sql::EVENT,params![input.request,"captured",stamp])?;
            let result=receipt(&tx,&input.request)?.ok_or_else(||anyhow::anyhow!("chat receipt {} vanished inside its own insert transaction",input.request))?;
            tx.commit()?;
            Ok(Admission::Saved(result))
        }).await
    }
    pub async fn recording_receipt(&self, request: String) -> Result<Option<Value>> {
        self.run(move |c| receipt(c, &request)).await
    }
    pub async fn cancellation_requested(&self, request: String) -> Result<bool> {
        self.run(move |c| {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM run_controls WHERE request_id=?1 AND cancel_requested_at IS NOT NULL)",
                [request],
                |r| r.get(0),
            )?)
        }).await
    }
    pub async fn request_cancellation(&self, request: String) -> Result<Option<Value>> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let found: Option<(String, String)> = tx.query_row(
                "SELECT state,session_id FROM chat_receipts WHERE request_id=?1",
                [&request],
                |r| Ok((r.get(0)?, r.get(1)?)),
            ).optional()?;
            let Some((state, session)) = found else { return Ok(None); };
            if RequestState::parse(&state).is_some_and(RequestState::is_pending) {
                let stamp = now();
                let already_requested: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM run_controls WHERE request_id=?1 AND cancel_requested_at IS NOT NULL)",
                    [&request],
                    |r| r.get(0),
                )?;
                tx.execute(
                    "INSERT INTO run_controls(request_id,cancel_requested_at) VALUES(?1,?2) ON CONFLICT(request_id) DO UPDATE SET cancel_requested_at=COALESCE(run_controls.cancel_requested_at,excluded.cancel_requested_at)",
                    params![request, stamp],
                )?;
                tx.execute(
                    "UPDATE permission_requests SET status='expired',resolved_at=?2 WHERE request_id=?1 AND status='pending'",
                    params![request, stamp],
                )?;
                if !already_requested {
                    tx.execute(
                        agentic::EVENT,
                        params![request, session, None::<String>, "cancel_requested", "{}", stamp],
                    )?;
                }
                if RequestState::parse(&state) == Some(RequestState::Captured) {
                    cancel_tx(&tx, &request, &session, &stamp)?;
                }
            }
            let result = receipt(&tx, &request)?;
            tx.commit()?;
            Ok(result)
        }).await
    }
    pub async fn finalize_cancellation(&self, request: String) -> Result<bool> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let cancelled = cancel_pending_tx(&tx, &request, &now())?;
            tx.commit()?;
            Ok(cancelled)
        })
        .await
    }
    pub async fn retry_recording(&self, request: String) -> Result<RetryAdmission> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let source: Option<RetrySource> = tx.query_row(
                "SELECT r.state,r.session_id,r.scope,r.model,m.content,r.redacted,r.provider_id,r.provider_version FROM chat_receipts r JOIN messages m ON m.id=r.request_id WHERE r.request_id=?1",
                [&request],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
            ).optional()?;
            let Some((state, session, scope, model, prompt, redacted, provider_id, provider_version)) = source else {
                return Ok(RetryAdmission::NotFound);
            };
            // Retryable is narrower than terminal: `complete` is also terminal,
            // and re-running a finished answer would bill for it twice.
            if !matches!(
                RequestState::parse(&state),
                Some(RequestState::Failed | RequestState::Interrupted)
            ) {
                return Ok(RetryAdmission::NotTerminal);
            }
            let prior_retry: Option<String> = tx.query_row(
                "SELECT retried_by FROM run_controls WHERE request_id=?1",
                [&request],
                |r| r.get(0),
            ).optional()?.flatten();
            if let Some(prior_retry) = prior_retry {
                return Ok(RetryAdmission::Saved(receipt(&tx, &prior_retry)?.ok_or_else(|| anyhow::anyhow!("retry lineage points to a missing receipt"))?));
            }
            if tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM chat_receipts WHERE session_id=?1 AND state IN ('captured','generating'))",
                [&session],
                |r| r.get::<_, bool>(0),
            )? {
                return Ok(RetryAdmission::Busy);
            }
            if tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM file_changes WHERE request_id=?1 AND applied=1)",
                [&request],
                |r| r.get::<_, bool>(0),
            )? {
                return Ok(RetryAdmission::Unsafe);
            }
            let registry = Registry::standard();
            let mut safe_boundary_seq = None::<i64>;
            {
                let mut stmt = tx.prepare(
                    "SELECT tool_name,input_json,status,seq FROM turn_steps WHERE request_id=?1 AND kind='tool_call' ORDER BY seq",
                )?;
                let rows = stmt.query_map([&request], |r| Ok((
                    r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?, r.get::<_, i64>(3)?,
                )))?;
                for row in rows {
                    let (name, input, status, seq) = row?;
                    let Some(tool) = name.as_deref().and_then(|name| registry.get(name)) else {
                        return Ok(RetryAdmission::Unsafe);
                    };
                    let args = input.as_deref().and_then(|value| serde_json::from_str::<Value>(value).ok()).unwrap_or(Value::Null);
                    if status != "denied" && tool.side_effecting_for(&args) {
                        return Ok(RetryAdmission::Unsafe);
                    }
                    if status == "complete" {
                        safe_boundary_seq = Some(seq);
                    }
                }
            }
            let retry = uid();
            let stamp = now();
            let signature = safety::fingerprint(&json!({"retry_of":request,"request":retry,"prompt":prompt}).to_string());
            tx.execute(sql::INSERT_MESSAGE, params![retry, session, prompt, stamp])?;
            tx.execute(sql::INSERT_RECEIPT, params![retry, session, scope, model, signature, redacted, stamp, provider_id, provider_version])?;
            tx.execute(sql::INSERT_OUTBOX, params![retry, stamp])?;
            tx.execute(sql::EVENT, params![retry, "captured", stamp])?;
            tx.execute(
                "INSERT INTO run_controls(request_id,retry_of) VALUES(?1,?2)",
                params![retry, request],
            )?;
            tx.execute(
                "INSERT INTO run_controls(request_id,safe_boundary_seq,retried_by) VALUES(?1,?2,?3) ON CONFLICT(request_id) DO UPDATE SET safe_boundary_seq=excluded.safe_boundary_seq,retried_by=excluded.retried_by",
                params![request, safe_boundary_seq, retry],
            )?;
            tx.execute(
                agentic::EVENT,
                params![retry, session, None::<String>, "retry_created", json!({"retry_of":request,"safe_boundary_seq":safe_boundary_seq}).to_string(), stamp],
            )?;
            let result = receipt(&tx, &retry)?.ok_or_else(|| anyhow::anyhow!("retry receipt was not created"))?;
            tx.commit()?;
            Ok(RetryAdmission::Saved(result))
        }).await
    }

    pub async fn recording_context(&self, request: String) -> Result<Option<Value>> {
        self.run(move |c| {
            let Some(mut result) = receipt(c, &request)? else {
                return Ok(None);
            };
            let context: Option<String> = c.query_row(
                "SELECT context_json FROM chat_receipts WHERE request_id=?1",
                [&request],
                |r| r.get(0),
            )?;
            result["context"] = context
                .map(|v| serde_json::from_str::<Value>(&v))
                .transpose()?
                .unwrap_or(Value::Null);
            let mut stmt = c.prepare(
                "SELECT kind,created_at FROM recording_events WHERE request_id=?1 ORDER BY seq",
            )?;
            result["events"] = json!(stmt
                .query_map([request], |r| Ok(
                    json!({"kind":r.get::<_,String>(0)?,"at":r.get::<_,String>(1)?})
                ))?
                .collect::<rusqlite::Result<Vec<_>>>()?);
            Ok(Some(result))
        })
        .await
    }
    pub async fn claim_recording(&self) -> Result<Option<Generation>> {
        let worker_id = self.worker_identity().as_str().to_string();
        let claimed = self
            .run(move |c| {
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row:Option<ClaimedRecording>=tx.query_row(
                "SELECT r.request_id,r.session_id,r.scope,r.model,r.provider_id,r.provider_version,m.content,m.seq FROM chat_receipts r JOIN messages m ON m.id=r.request_id WHERE r.state='captured' ORDER BY m.seq LIMIT 1",[],
                |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional()?;
            let Some((request,session,scope,model,provider_id,provider_version,prompt,seq))=row else {return Ok(None)};
            // Admission permits one unfinished turn per session, so completed pairs
            // remain ordered. Other sessions can queue independently.
            let mut events={
                let mut stmt=tx.prepare("SELECT m.id,m.role,m.content FROM messages m WHERE m.session_id=?1 AND m.status='complete' AND m.seq<?2 ORDER BY m.seq DESC LIMIT 20")?;
                let mut rows=stmt.query_map(params![session,seq],|r|Ok(Event{id:r.get(0)?,role:r.get(1)?,content:r.get(2)?}))?.collect::<rusqlite::Result<Vec<_>>>()?;
                rows.reverse(); rows
            };
            while events.first().is_some_and(|e|e.role!="user") {events.remove(0);}
            events.push(Event{id:request.clone(),role:"user".into(),content:prompt.clone()});
            if tx.execute(sql::CLAIM,params![request,now()])?!=1 {bail!("recording was not captured");}
            // Ownership is taken in the same transaction as the claim. A claim that committed
            // without a lease would leave a turn whose state says "being worked on" and whose
            // lease says nobody owns it -- the exact ambiguity a second worker cannot resolve.
            let lease = match crate::storage::acquire_in_tx(&tx, &request, &worker_id)? {
                Ok(lease) => lease,
                // A turn the restart sweep reset to `captured` still carries the dead worker's
                // lease row, so a fresh process is never the holder. Without a takeover that turn
                // would be permanently unclaimable. Stealing is attempted only once the lease has
                // lapsed, and `steal_in_tx` refuses it outright if the old holder left an external
                // effect in flight -- in that case the turn waits for a human, which is the point.
                Err(crate::storage::AcquireRefusal::HeldByAnother { lapsed: true, .. }) => {
                    match crate::storage::steal_in_tx(&tx, &request, &worker_id)? {
                        Ok(lease) => {
                            tx.execute(sql::EVENT, params![request, "lease_stolen", now()])?;
                            lease
                        }
                        Err(refusal) => {
                            // Committed, not rolled back: on the effects path `steal_in_tx` has
                            // just recorded the unknown outcomes a human needs to see.
                            tx.execute(sql::EVENT, params![request, "lease_steal_refused", now()])?;
                            tx.commit()?;
                            bail!("cannot take over the lease on {request}: {refusal:?}");
                        }
                    }
                }
                Err(refusal) => bail!("cannot take the lease on {request}: {refusal:?}"),
            };
            tx.execute(sql::EVENT,params![request,"generation_started",now()])?;
            tx.commit()?;
            Ok(Some((Generation{request,session,scope,model,provider_id,provider_version,prompt,events}, lease)))
        }).await?;
        // Remembered after the commit: the fence this process must present on every later write
        // for this turn. Re-reading the row at write time would authorize a holder the database
        // has already moved past, which is the stale writer this exists to refuse.
        Ok(claimed.map(|(turn, lease)| {
            self.remember_lease(lease);
            turn
        }))
    }
    pub async fn save_recording_context(&self, request: String, context: Value) -> Result<()> {
        // `None` means this process never claimed the turn: cancellation arriving over HTTP and
        // the restart recovery sweep are not lease holders and are not treated as one.
        let lease = self.held_lease(&request);
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(lease) = &lease {
                crate::storage::guard_fence(&tx, lease)?;
            }
            if tx.execute(sql::CONTEXT, params![request, context.to_string(), now()])? != 1 {
                bail!("context cannot be saved in this state");
            }
            tx.execute(sql::EVENT, params![request, "context_saved", now()])?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn complete_recording(&self, request: String, answer: String) -> Result<()> {
        let lease = self.held_lease(&request);
        let finished = request.clone();
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            // The fence is checked in the same transaction as the answer write, so a holder whose
            // lease was taken over cannot land an answer next to the new holder's.
            if let Some(lease) = &lease {
                crate::storage::guard_fence(&tx, lease)?;
            }
            if cancel_pending_tx(&tx, &request, &stamp)? {
                tx.commit()?;
                return Ok(());
            }
            let session: String = tx.query_row(
                "SELECT session_id FROM chat_receipts WHERE request_id=?1 AND state='generating'",
                [&request],
                |r| r.get(0),
            )?;
            let answer_id = uid();
            if tx.execute(sql::COMPLETE_USER, params![request])? != 1 {
                bail!("user message not pending");
            }
            tx.execute(sql::ANSWER, params![answer_id, session, answer, stamp])?;
            if tx.execute(sql::COMPLETE, params![request, answer_id, stamp])? != 1 {
                bail!("generation not ready to complete");
            }
            tx.execute(sql::EVENT, params![request, "answer_saved", stamp])?;
            // The activity feed is written here so a saved answer and its `answer_saved` row
            // can never disagree about whether this turn finished.
            tx.execute(
                agentic::EVENT,
                params![
                    request,
                    session,
                    None::<String>,
                    "answer_saved",
                    "{}",
                    stamp
                ],
            )?;
            // Publish only the terminal event here. Answer chunks are already durable: every
            // completed line was committed by the sink before delivery (P7-T05), so re-writing
            // the answer would duplicate it. `answer` is still stored on the answer message.
            let _ = &answer;
            tx.execute(
                "INSERT INTO generation_events(request_id,session_id,state,content,error_code,created_at) VALUES(?1,?2,'completed','',NULL,?3)",
                params![request, session, stamp],
            )?;
            // No job insert here. A full/failed extraction queue cannot undo this answer.
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.release_held_lease(&finished).await;
        Ok(())
    }
    pub async fn fail_recording(&self, request: String, code: &'static str) -> Result<()> {
        if ![
            "context_failed",
            "provider_failed",
            "generation_stream_save_failed",
            "answer_save_failed",
            "worker_failed",
        ]
        .contains(&code)
        {
            bail!("invalid failure code");
        }
        let lease = self.held_lease(&request);
        let finished = request.clone();
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            // A failure verdict is a durable write too: recording "this turn failed" on a turn
            // another worker now owns would overwrite a live generation with a stale verdict.
            if let Some(lease) = &lease {
                crate::storage::guard_fence(&tx, lease)?;
            }
            if cancel_pending_tx(&tx, &request, &stamp)? {
                tx.commit()?;
                return Ok(());
            }
            if tx.execute(sql::FAIL, params![request, code, stamp])? == 1 {
                tx.execute(sql::FAIL_MESSAGE, [&request])?;
                tx.execute(sql::EVENT, params![request, "generation_failed", stamp])?;
                tx.execute(
                    "INSERT INTO generation_events(request_id,session_id,state,content,error_code,created_at) SELECT request_id,session_id,'failed','',?2,?3 FROM chat_receipts WHERE request_id=?1",
                    params![request, code, stamp],
                )?;
                // Every failure path, including the worker's panic guard, lands in the feed.
                let session: Option<String> = tx
                    .query_row(agentic::SESSION_OF_REQUEST, [&request], |r| r.get(0))
                    .optional()?;
                if let Some(session) = session {
                    tx.execute(
                        agentic::EVENT,
                        params![
                            request,
                            session,
                            None::<String>,
                            "turn_failed",
                            json!({"error_code":code}).to_string(),
                            stamp
                        ],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.release_held_lease(&finished).await;
        Ok(())
    }
    pub async fn flush_recording_outbox(&self) -> Result<usize> {
        self.run(|c| {
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let queued:i64=tx.query_row("SELECT count(*) FROM jobs WHERE status IN ('pending','running')",[],|r|r.get(0))?;
            let capacity=(1000-queued).clamp(0,32);
            let rows={
                let mut stmt=tx.prepare("SELECT o.request_id,r.scope,r.session_id,m.content FROM recording_outbox o JOIN chat_receipts r ON r.request_id=o.request_id JOIN messages m ON m.id=o.request_id WHERE o.job_id IS NULL AND r.state IN ('complete','failed','interrupted') ORDER BY m.seq LIMIT ?1")?;
                let rows=stmt.query_map([capacity],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?; rows
            };
            for (request,scope,session,prompt) in &rows {
                let job=uid();let source=format!("chat:{request}");let stamp=now();
                let plan={
                    let mut stmt=tx.prepare("SELECT seq,status,text FROM plan_items WHERE session_id=?1 ORDER BY seq")?;
                    let collected=stmt.query_map([session],|r|Ok(json!({"seq":r.get::<_,i64>(0)?,"status":r.get::<_,String>(1)?,"text":r.get::<_,String>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    collected
                };
                let mut events=vec![Event{id:request.clone(),role:"user".into(),content:prompt.clone()}];
                if !plan.is_empty(){events.push(Event{id:format!("plan:{request}"),role:"plan".into(),content:serde_json::to_string(&plan)?});}
                tx.execute(sql::ENQUEUE,params![job,source,scope,source,serde_json::to_string(&events)?,chrono::Utc::now().timestamp(),stamp])?;
                if tx.execute(sql::LINK_JOB,params![request,job])?!=1 {bail!("outbox already dispatched");}
                tx.execute(sql::EVENT,params![request,"extraction_queued",stamp])?;
            }
            let count=rows.len();tx.commit()?;Ok(count)
        }).await
    }
    pub async fn history(&self, session: String, before: Option<i64>) -> Result<Value> {
        self.run(move|c| {
            let scope:Option<String>=c.query_row("SELECT scope FROM sessions WHERE id=?1",[&session],|r|r.get(0)).optional()?;
            let mut stmt=c.prepare("SELECT m.seq,m.id,m.role,m.content,m.status,r.state,COALESCE(r.request_id,a.request_id),COALESCE(r.error_code,a.error_code) FROM messages m LEFT JOIN chat_receipts r ON r.request_id=m.id LEFT JOIN chat_receipts a ON a.answer_id=m.id WHERE m.session_id=?1 AND m.seq<?2 ORDER BY m.seq DESC LIMIT 101")?;
            let mut rows=stmt.query_map(params![session,before.unwrap_or(i64::MAX)],|r|Ok(json!({"seq":r.get::<_,i64>(0)?,"id":r.get::<_,String>(1)?,"role":r.get::<_,String>(2)?,"content":r.get::<_,String>(3)?,"status":r.get::<_,String>(4)?,"generation_state":r.get::<_,Option<String>>(5)?,"request_id":r.get::<_,Option<String>>(6)?,"error_code":r.get::<_,Option<String>>(7)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more=rows.len()>100;rows.truncate(100);rows.reverse();
            let cursor=if more {rows.first().and_then(|r|r["seq"].as_i64())} else {None};
            Ok(json!({"scope":scope,"messages":rows,"has_more":more,"next_before_seq":cursor}))
        }).await
    }
    #[cfg(test)]
    pub async fn recorded_sessions(&self, before: Option<i64>) -> Result<Value> {
        self.recorded_sessions_filtered(before, None, false).await
    }
    pub async fn recorded_sessions_filtered(
        &self,
        before: Option<i64>,
        search: Option<String>,
        include_archived: bool,
    ) -> Result<Value> {
        self.run(move|c| {
            let pattern = search.as_deref().map(|value| format!("%{}%", value.to_lowercase()));
            let mut stmt=c.prepare("SELECT s.id,s.scope,s.created_at,s.title,s.archived_at,s.forked_from,MAX(m.seq),COUNT(m.id),COALESCE((SELECT substr(content,1,100) FROM messages WHERE session_id=s.id AND role='user' ORDER BY seq LIMIT 1),'Conversation') FROM sessions s JOIN messages m ON m.session_id=s.id WHERE (?1 IS NULL OR lower(s.title) LIKE ?1 OR EXISTS(SELECT 1 FROM messages sm WHERE sm.session_id=s.id AND lower(sm.content) LIKE ?1)) AND (?2=1 OR s.archived_at IS NULL) GROUP BY s.id HAVING MAX(m.seq)<?3 ORDER BY MAX(m.seq) DESC LIMIT 51")?;
            let mut rows=stmt.query_map(params![pattern,include_archived,before.unwrap_or(i64::MAX)],|r|Ok(json!({"id":r.get::<_,String>(0)?,"scope":r.get::<_,String>(1)?,"created_at":r.get::<_,String>(2)?,"title":r.get::<_,String>(3)?,"archived_at":r.get::<_,Option<String>>(4)?,"forked_from":r.get::<_,Option<String>>(5)?,"last_seq":r.get::<_,i64>(6)?,"message_count":r.get::<_,i64>(7)?,"preview":r.get::<_,String>(8)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more=rows.len()>50;rows.truncate(50);
            let cursor=if more {rows.last().and_then(|r|r["last_seq"].as_i64())} else {None};
            Ok(json!({"sessions":rows,"has_more":more,"next_before_seq":cursor}))
        }).await
    }

    pub async fn update_session(
        &self,
        session: String,
        title: Option<String>,
        archived: Option<bool>,
    ) -> Result<Option<Value>> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)", [&session], |r| r.get(0))?;
            if !exists { return Ok(None); }
            let stamp = now();
            if let Some(title) = title { tx.execute("UPDATE sessions SET title=?2 WHERE id=?1", params![session, title])?; }
            if let Some(archived) = archived { tx.execute("UPDATE sessions SET archived_at=?2 WHERE id=?1", params![session, if archived { Some(stamp.clone()) } else { None }])?; }
            let row = tx.query_row("SELECT id,scope,title,archived_at,forked_from FROM sessions WHERE id=?1", [&session], |r| Ok(json!({"id":r.get::<_,String>(0)?,"scope":r.get::<_,String>(1)?,"title":r.get::<_,String>(2)?,"archived_at":r.get::<_,Option<String>>(3)?,"forked_from":r.get::<_,Option<String>>(4)?})))?;
            tx.commit()?;
            Ok(Some(row))
        }).await
    }

    pub async fn fork_session(
        &self,
        source: String,
        title: Option<String>,
    ) -> Result<Option<Value>> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let source_row: Option<(String,String,String)> = tx.query_row("SELECT scope,title,created_at FROM sessions WHERE id=?1", [&source], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let Some((scope, source_title, _)) = source_row else { return Ok(None); };
            let new_id = uid();
            let new_title = title.unwrap_or_else(|| format!("{} copy", source_title));
            let stamp = now();
            tx.execute("INSERT INTO sessions(id,scope,created_at,title,forked_from) VALUES(?1,?2,?3,?4,?5)", params![new_id,scope,stamp,new_title,source])?;
            let mut stmt = tx.prepare("SELECT role,content,status,created_at FROM messages WHERE session_id=?1 ORDER BY seq")?;
            let rows = stmt.query_map([&source], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            for (role,content,status,created_at) in rows {
                tx.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?1,?2,?3,?4,CASE WHEN ?5='complete' THEN 'complete' ELSE 'failed' END,?6)", params![uid(),new_id,role,content,status,created_at])?;
            }
            let result = json!({"id":new_id,"scope":scope,"title":new_title,"archived_at":null,"forked_from":source});
            tx.commit()?;
            Ok(Some(result))
        }).await
    }
}

/// Prepare the turn, persist the window once, then hand the conversation to the agent loop.
/// This function owns the receipt state machine; `agent_loop` owns steps, tools and events.
pub(crate) async fn generate(
    store: &DbStore,
    agents: &MemoryAgents,
    turn: Generation,
) -> Result<()> {
    let total_started = Instant::now();
    let context_started = Instant::now();
    let observer = crate::runtime_observability::RuntimeObserver::default();
    if store.cancellation_requested(turn.request.clone()).await? {
        store.finalize_cancellation(turn.request).await?;
        return Ok(());
    }
    // P15-T02: recall reports what it considered as well as what it returned, so the turn can
    // persist why each candidate was kept or dropped instead of only the survivors.
    let strategy = crate::embeddings::strategy_from_env();
    let (recalled, mut retrieval): (Vec<Recall>, Vec<crate::storage::RecallCandidate>) = match store
        .recall_explained(turn.scope.clone(), turn.prompt.clone(), strategy)
        .await
    {
        Ok(pair) => pair,
        Err(_) => return store.fail_recording(turn.request, "context_failed").await,
    };
    // The scope decides whether this turn has tools at all (P1-T04). Without a configured
    // `root_path` the turn stays on the text-only path instead of guessing a project root.
    let scope = match store.scope_config(turn.scope.clone()).await {
        Ok(found) => found.unwrap_or_else(|| ScopeConfig::blank(&turn.scope)),
        Err(_) => return store.fail_recording(turn.request, "context_failed").await,
    };
    // Both project-derived producers run in one blocking hop: the bounded repository map (P3-T04)
    // and the skills index (P5-T02). Either one failing is `context_failed`, because a turn that
    // silently dropped them would look identical to a project that has neither.
    let (repo_parts, skill_parts) = if let Some(root) = scope.root_path.clone() {
        let produced = tokio::task::spawn_blocking(move || {
            let root = std::path::Path::new(&root);
            let map = crate::repo_map::load_or_refresh(root)?;
            Ok::<_, anyhow::Error>((map, crate::skills::index_parts(root)?))
        })
        .await;
        match produced {
            Ok(Ok((map, skills))) => (
                vec![context::NamedPart {
                    id: map.id,
                    text: map.text,
                }],
                skills
                    .into_iter()
                    .map(|(id, text)| context::NamedPart { id, text })
                    .collect::<Vec<_>>(),
            ),
            _ => return store.fail_recording(turn.request, "context_failed").await,
        }
    } else {
        (Vec::new(), Vec::new())
    };
    let plan = match store.plan(turn.session.clone()).await {
        Ok(plan) => plan,
        Err(_) => return store.fail_recording(turn.request, "context_failed").await,
    };
    let offered_tools = if scope.root_path.is_some() {
        match Registry::standard().schemas() {
            Ok(tools) => tools,
            Err(_) => return store.fail_recording(turn.request, "context_failed").await,
        }
    } else {
        Vec::new()
    };
    let Some((user_message, recent_steps)) = turn.events.split_last() else {
        return store.fail_recording(turn.request, "context_failed").await;
    };
    if user_message.id != turn.request {
        return store.fail_recording(turn.request, "context_failed").await;
    }
    let sources = context::Sources {
        skills_index: skill_parts,
        repo_map: repo_parts,
        ..context::Sources::default()
    };
    let built = match context::build(context::BuildInput {
        scope: &scope,
        tools: &offered_tools,
        sources: &sources,
        memories: &recalled,
        plan: &plan,
        recent_steps,
        user_message,
        budgets: context::Budgets::default(),
    }) {
        Ok(window) => window,
        Err(_) => return store.fail_recording(turn.request, "context_failed").await,
    };
    let context::Window {
        messages,
        tools,
        memories,
        receipt: context_receipt,
    } = built;
    let agentic_turn = !tools.is_empty();
    // Recall admitted these rows; the context builder is what dropped any of them, so the
    // recorded reason names the budget rather than the ranking.
    let sent = memories
        .iter()
        .map(|memory| memory.id.clone())
        .collect::<std::collections::HashSet<_>>();
    for row in retrieval.iter_mut() {
        if row.decision == "included" && !sent.contains(&row.id) {
            row.excluded_by_context_budget();
        }
    }
    if store
        .save_retrieval_receipt(
            turn.request.clone(),
            turn.scope.clone(),
            strategy,
            turn.prompt.clone(),
            context::Budgets::default().recalled_memories as i64,
            retrieval,
        )
        .await
        .is_err()
    {
        return store.fail_recording(turn.request, "context_failed").await;
    }
    let receipt = json!({"format_version":2,"adapter":if agentic_turn {"tool_calls_v1"} else {"text_completion_v1"},
        "model":turn.model,"provider":{"id":turn.provider_id,"version":turn.provider_version},
        "provider_messages":messages.clone(),"provider_tools":tools.clone(),"memories":memories,
        "context_receipt":context_receipt,
        "scope":{"root_path":scope.root_path.clone(),"permission_mode":scope.permission_mode.clone(),"diagnostics_cmd":scope.diagnostics_cmd.clone(),"tools_enabled":agentic_turn},
        "note":"Exact sanitized message and tool arrays prepared for the provider's FIRST call in this turn, not model reasoning or proof of provider receipt. Later calls append tool results; each one stores its own full message array and tool names in turn_steps.input_json."});
    if store
        .save_recording_context(turn.request.clone(), receipt)
        .await
        .is_err()
    {
        return store.fail_recording(turn.request, "context_failed").await;
    }
    if store.cancellation_requested(turn.request.clone()).await? {
        store.finalize_cancellation(turn.request).await?;
        return Ok(());
    }
    // No provider call is allowed before context persistence succeeds.
    // The sink persists each redacted chunk before delivery, so an incremental answer is durable
    // before any client can observe it.
    observer.context(context_started.elapsed());
    let mut sink = RecordingGenerationSink::new(
        store,
        turn.request.clone(),
        turn.session.clone(),
        observer.clone(),
    );
    let provider_work = agent_loop::run(
        agent_loop::Turn {
            store,
            agents,
            request: turn.request.clone(),
            session: turn.session.clone(),
            model: turn.model.clone(),
            scope,
            messages,
            tools,
            observer: observer.clone(),
        },
        &mut sink,
    );
    let outcome = agents
        .within_spend_request(turn.request.clone(), provider_work)
        .await?;
    if store.cancellation_requested(turn.request.clone()).await? {
        store.finalize_cancellation(turn.request).await?;
        return Ok(());
    }
    if let Some(code) = sink.failure() {
        // A chunk that could not be made durable ends the turn explicitly rather than silently
        // truncating the answer the reader already saw.
        return store.fail_recording(turn.request, code).await;
    }
    let answer = match outcome {
        agent_loop::Outcome::Answer(text) => safety::redact(&text),
        agent_loop::Outcome::ProviderFailed => {
            return store.fail_recording(turn.request, "provider_failed").await
        }
    };
    let publication_started = Instant::now();
    if store
        .complete_recording(turn.request.clone(), answer)
        .await
        .is_err()
    {
        return store
            .fail_recording(turn.request, "answer_save_failed")
            .await;
    }
    observer.publication(publication_started.elapsed());
    observer.emit(total_started.elapsed(), "complete");
    Ok(())
}

pub async fn worker(
    store: DbStore,
    providers: ProviderRegistry,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        match store.claim_recording().await {
            Ok(Some(turn)) => {
                let id = turn.request.clone();
                // The lease taken by the claim. Renewed while the turn runs so a live worker's
                // ownership does not lapse mid-generation, which is what would invite a takeover
                // of a turn that is still making progress.
                let lease = store.held_lease(&id);
                let db = store.clone();
                let provider_registry = providers.clone();
                let provider_id = turn.provider_id.clone();
                let provider_version = turn.provider_version;
                // Observe panics as well as returned errors; no HTTP request owns this work.
                let mut task = tokio::spawn(async move {
                    let provider = match provider_registry
                        .agents_for(&provider_id, provider_version)
                        .await
                    {
                        Ok(provider) => provider,
                        Err(_) => {
                            db.fail_recording(turn.request.clone(), "provider_config_failed")
                                .await?;
                            return Ok(());
                        }
                    };
                    generate(&db, &provider, turn).await
                });
                let result = loop {
                    // A third of the TTL: two consecutive missed heartbeats still leave the lease
                    // valid, so a single slow tick does not cost a working turn its ownership.
                    let beat = std::time::Duration::from_secs(
                        (crate::storage::LEASE_TTL_SECONDS as u64).div_ceil(3),
                    );
                    tokio::select! {
                        finished = &mut task => break finished,
                        _ = tokio::time::sleep(beat) => {
                            if let Some(lease) = &lease {
                                // A lost lease is reported, not acted on: the generation is not
                                // aborted here because its durable writes are already refused by
                                // the fence check, and tearing down a turn that may still be
                                // mid-effect is the steal path's decision, not the heartbeat's.
                                if !matches!(store.renew_lease(lease).await, Ok(true)) {
                                    eprintln!("{{\"event\":\"lease_renewal_failed\"}}");
                                }
                            }
                        }
                    }
                };
                if !matches!(result, Ok(Ok(()))) {
                    eprintln!("{{\"event\":\"generation_worker_failed\"}}");
                    if store.fail_recording(id, "worker_failed").await.is_err() {
                        eprintln!("{{\"event\":\"recording_failure_persistence_failed\"}}");
                        // Stop generation rather than process newer turns after an unknown save.
                        return;
                    }
                }
            }
            Ok(None) => tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {},
                changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() { return; },
            },
            Err(_) => {
                eprintln!("{{\"event\":\"recording_claim_failed\"}}");
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {},
                    changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() { return; },
                }
            }
        }
    }
}
