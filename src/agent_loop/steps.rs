//! Durable step and permission transitions: the write half of the agentic turn loop.
//!
//! Moved verbatim from `agent_loop.rs` by P12-T01; behaviour is unchanged. The rule these
//! methods exist to enforce is stated in the parent module: a step row is committed before the
//! side effect it describes, and every artifact is persisted in the transaction that finishes
//! the step, so an applied edit and its audit row cannot drift apart.
use crate::{
    agentic_sql as sql,
    storage::{now, uid, DbStore},
    tools::Artifact,
};
use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

// ---- Durable transitions ---------------------------------------------------------------

/// What a human decision did to a pending approval. The HTTP layer maps these to status codes;
/// the loop never sees them, because it only reads the stored `status`.
pub enum Resolution {
    /// This call wrote the decision and its `permission_resolved` event.
    Recorded,
    /// The same decision was already on record: a replayed click, not an error.
    Unchanged,
    /// Already resolved the other way. Nothing was changed.
    Conflict,
    /// The turn stopped waiting before the decision arrived.
    Expired,
    /// No such approval in this scope.
    NotFound,
}

/// A `running` step plus the activity event that announces it.
pub struct NewStep {
    pub request: String,
    pub session: String,
    pub kind: &'static str,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<String>,
    pub input: Value,
    pub event: &'static str,
    pub payload: Value,
}

/// A finished step, the side effects it asked to have recorded, and its activity event.
pub struct StepOutcome {
    pub step: String,
    pub request: String,
    pub session: String,
    pub status: &'static str,
    pub output: Value,
    pub bytes: i64,
    pub truncated: bool,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub error_code: Option<String>,
    pub event: &'static str,
    pub payload: Value,
    pub artifacts: Vec<Artifact>,
}

/// A pending approval: which call is asking, what the human will be shown, and how long the
/// asking turn will actually wait. Named fields, because `request`/`session`/`step` are three
/// interchangeable-looking `String`s that a positional call could silently transpose.
pub struct NewPermission {
    pub request: String,
    pub session: String,
    pub step: String,
    pub tool: String,
    pub summary: String,
    pub args: Value,
    /// The caller's effective deadline (see `permission_ttl`), not a wish.
    pub ttl_seconds: i64,
}

impl DbStore {
    /// Commit a `running` step and its `*_started` event together, and return the step id.
    /// The sequence number comes from the same transaction, so two steps of one request can
    /// never share a `seq`.
    pub async fn begin_step(&self, step: NewStep) -> Result<String> {
        self.insert_step(None, step).await
    }

    /// P5-T03: a step a sub-agent owns. Same request, same `seq` sequence and same event feed as
    /// the main loop's steps; `parent_step_id` is the `task` tool-call step, so the sub-agent's
    /// work reads as its own list instead of being mistaken for the parent's.
    pub async fn begin_child_step(&self, parent: String, step: NewStep) -> Result<String> {
        self.insert_step(Some(parent), step).await
    }

    async fn insert_step(&self, parent: Option<String>, step: NewStep) -> Result<String> {
        let id = uid();
        let created = id.clone();
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let seq: i64 = tx.query_row(sql::STEP_NEXT_SEQ, [&step.request], |r| r.get(0))?;
            let stamp = now();
            tx.execute(
                sql::STEP_BEGIN,
                params![
                    id,
                    step.request,
                    parent,
                    seq,
                    step.kind,
                    step.tool_name,
                    step.tool_call_id,
                    step.input.to_string(),
                    stamp
                ],
            )?;
            tx.execute(
                sql::EVENT,
                params![
                    step.request,
                    step.session,
                    id,
                    step.event,
                    step.payload.to_string(),
                    stamp
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        Ok(created)
    }

    /// Finish a step, persist its artifacts and announce all of it in one transaction.
    /// `file_changes` rows are written with `applied=1`: the tool has already written the file
    /// by the time it hands back the artifact, so claiming anything else would be a lie.
    pub async fn finish_step(&self, done: StepOutcome) -> Result<()> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            if tx.execute(sql::STEP_FINISH, params![done.step, done.status, done.output.to_string(), done.bytes, done.truncated, done.tokens_in, done.tokens_out, done.error_code, stamp])? != 1 {
                bail!("step {} is no longer running", done.step);
            }
            if done.error_code.as_deref() == Some("stale_anchor") {
                let prior_read: Option<String> = tx.query_row(
                    "SELECT prior.id FROM turn_steps current JOIN turn_steps prior ON prior.request_id=current.request_id AND prior.seq<current.seq WHERE current.id=?1 AND prior.kind='tool_call' AND prior.tool_name='read' AND prior.status='complete' AND json_extract(prior.input_json,'$.path')=json_extract(current.input_json,'$.path') ORDER BY prior.seq DESC LIMIT 1",
                    [&done.step], |r| r.get(0),
                ).optional()?;
                if let Some(read_step) = prior_read {
                    tx.execute(sql::PROVENANCE_EDGE_INSERT,
                        params![uid(), done.request, "step", read_step, "contradicts", "step", done.step, stamp])?;
                }
            }
            tx.execute(sql::EVENT, params![done.request, done.session, done.step, done.event, done.payload.to_string(), stamp])?;
            for artifact in &done.artifacts {
                match artifact {
                    Artifact::FileChange { path, action, before_hash, after_hash, diff, plus, minus } => {
                        let change = uid();
                        tx.execute(sql::FILE_CHANGE, params![change, done.request, done.step, path, action, before_hash, after_hash, diff, 1, stamp])?;
                        tx.execute(sql::PROVENANCE_EDGE_INSERT,
                            params![uid(), done.request, "step", done.step, "mutates", "mutation", change, stamp])?;
                        tx.execute(sql::EVENT, params![done.request, done.session, done.step, "file_changed",
                            json!({"change_id":change,"path":path,"action":action,"plus":plus,"minus":minus}).to_string(), stamp])?;
                    }
                    Artifact::Plan { items } => {
                        let stored = crate::storage::write_plan(&tx, &done.session, items)?;
                        tx.execute(sql::EVENT, params![done.request, done.session, done.step, "plan_updated", stored.to_string(), stamp])?;
                    }
                }
            }
            tx.commit()?;
            Ok(())
        }).await
    }

    /// An activity row for a transition that is not a step: turn start, budget stop.
    pub async fn activity(
        &self,
        request: String,
        session: String,
        kind: &'static str,
        payload: Value,
    ) -> Result<()> {
        self.run(move |c| {
            c.execute(
                sql::EVENT,
                params![
                    request,
                    session,
                    None::<String>,
                    kind,
                    payload.to_string(),
                    now()
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Create the `pending` approval a side-effecting call needs, with its event. The summary
    /// and payload come from the tool, never from model text.
    /// The row expires when the turn stops waiting, so the UI and the loop agree on the window.
    pub async fn request_permission(&self, ask: NewPermission) -> Result<String> {
        let id = uid();
        let created = id.clone();
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            let expires = (chrono::Utc::now() + chrono::Duration::seconds(ask.ttl_seconds)).to_rfc3339();
            tx.execute(sql::PERMISSION_CREATE, params![id, ask.request, ask.step, ask.tool, ask.summary, ask.args.to_string(), stamp, expires])?;
            tx.execute(sql::PROVENANCE_EDGE_INSERT, params![uid(), ask.request, "step", ask.step, "depends_on", "permission", id, stamp])?;
            tx.execute(sql::EVENT, params![ask.request, ask.session, ask.step, "permission_requested",
                json!({"permission_id":id,"tool":ask.tool,"summary":ask.summary,"expires_at":expires}).to_string(), stamp])?;
            tx.commit()?;
            Ok(())
        }).await?;
        Ok(created)
    }

    /// The stored decision, or `None` if the row is gone.
    pub async fn permission_status(&self, id: String) -> Result<Option<String>> {
        self.run(move |c| {
            Ok(
                c.query_row(sql::PERMISSION_STATUS, [id], |r| r.get::<_, String>(0))
                    .optional()?,
            )
        })
        .await
    }

    /// Every approval still waiting for a human in this scope, newest last. `args_json` is the
    /// tool's own bounded, redacted payload (a diff preview, a command), so the UI can show what
    /// it is about to allow without asking the model to describe it.
    pub async fn pending_permissions(&self, scope: String) -> Result<Value> {
        self.run(move |c| {
            let mut stmt = c.prepare(sql::PERMISSIONS_PENDING)?;
            let rows = stmt.query_map([scope], |r| Ok(json!({
                "id": r.get::<_, String>(0)?, "request_id": r.get::<_, String>(1)?, "step_id": r.get::<_, String>(2)?,
                "tool": r.get::<_, String>(3)?, "summary": r.get::<_, String>(4)?,
                "args": serde_json::from_str::<Value>(&r.get::<_, String>(5)?).unwrap_or(Value::Null),
                "created_at": r.get::<_, String>(6)?, "expires_at": r.get::<_, String>(7)?,
            })))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({"permissions": rows}))
        }).await
    }

    /// Record a human decision on a pending approval. The `pending` guard in `PERMISSION_RESOLVE`
    /// makes this idempotent: the first decision wins, a repeat of that same decision is accepted
    /// and writes no second event, and the other decision is a conflict rather than a silent flip.
    /// The waiting loop only ever reads `status`, so committing here is what unblocks the turn.
    pub async fn resolve_permission(
        &self,
        id: String,
        scope: String,
        decision: &'static str,
    ) -> Result<Resolution> {
        debug_assert!(
            matches!(decision, "approved" | "denied"),
            "only a human approve/deny reaches this"
        );
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row = tx
                .query_row(sql::PERMISSION_GET, [&id], |r| {
                    Ok((
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(10)?,
                    ))
                })
                .optional()?;
            // A wrong scope is a miss, not a leak: nothing tells the caller the row exists.
            let Some((request, step, status, _)) = row.filter(|row| row.3 == scope) else {
                return Ok(Resolution::NotFound);
            };
            match status.as_str() {
                "pending" => {}
                "expired" => return Ok(Resolution::Expired),
                same if same == decision => return Ok(Resolution::Unchanged),
                _ => return Ok(Resolution::Conflict),
            }
            let stamp = now();
            if tx.execute(sql::PERMISSION_RESOLVE, params![id, decision, stamp])? != 1 {
                bail!("approval {id} stopped being pending inside its own transaction");
            }
            tx.execute(
                sql::PROVENANCE_EDGE_INSERT,
                params![uid(), request, "permission", id,
                    if decision == "approved" { "authorizes" } else { "triggers" },
                    "step", step, stamp],
            )?;
            let session: String =
                tx.query_row(sql::SESSION_OF_REQUEST, [&request], |r| r.get(0))?;
            tx.execute(
                sql::EVENT,
                params![
                    request,
                    session,
                    step,
                    "permission_resolved",
                    json!({"permission_id":id,"decision":decision}).to_string(),
                    stamp
                ],
            )?;
            tx.commit()?;
            Ok(Resolution::Recorded)
        })
        .await
    }

    /// Expire a still-pending approval when the loop stops waiting. Idempotent: a decision that
    /// arrived first wins, and only a real expiry writes `permission_resolved`.
    pub async fn expire_permission(
        &self,
        id: String,
        request: String,
        session: String,
        step: String,
    ) -> Result<String> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            if tx.execute(sql::PERMISSION_EXPIRE, params![id, stamp])? == 1 {
                tx.execute(
                    sql::EVENT,
                    params![
                        request,
                        session,
                        step,
                        "permission_resolved",
                        json!({"permission_id":id,"decision":"expired"}).to_string(),
                        stamp
                    ],
                )?;
            }
            let status: String = tx.query_row(sql::PERMISSION_STATUS, [&id], |r| r.get(0))?;
            tx.commit()?;
            Ok(status)
        })
        .await
    }
}
