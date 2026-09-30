//! Commit evidence, receipt and bounded memory-extraction intent atomically.
use super::DbStore;
use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

const MAX_EXTERNAL_MEMORY_TEXT_BYTES: usize = 8 * 1024;
const MAX_PENDING_EXTERNAL_MEMORY_JOBS: i64 = 1000;

pub(super) enum ExternalMemoryInput {
    NotEligible,
    OverBudget,
    Eligible(crate::ingest::Event),
}

/// Only complete, client-supplied user messages can enter memory extraction. Tool results,
/// task output, assistant messages, summaries and truncated dialogue remain history evidence.
pub(super) fn external_memory_input(e: &Value) -> ExternalMemoryInput {
    if e["event_type"] != "message.observed"
        || e["capture"]["conversation"] != "client_supplied"
        || e["capture"]["payload"] != "full"
        || e["capture"]["truncated"] == true
        || e["payload"]["role"] != "user"
    {
        return ExternalMemoryInput::NotEligible;
    }
    let Some(text) = e["payload"]["text"].as_str() else {
        return ExternalMemoryInput::NotEligible;
    };
    if text.trim().is_empty() {
        return ExternalMemoryInput::NotEligible;
    }
    if text.len() > MAX_EXTERNAL_MEMORY_TEXT_BYTES {
        return ExternalMemoryInput::OverBudget;
    }
    let Some(id) = e["event_id"].as_str() else {
        return ExternalMemoryInput::NotEligible;
    };
    ExternalMemoryInput::Eligible(crate::ingest::Event {
        id: id.to_string(),
        role: "external_user".into(),
        content: text.to_string(),
    })
}

fn extraction_status(
    tx: &rusqlite::Transaction<'_>,
    job_key: &str,
    event: &Value,
) -> Result<&'static str> {
    let status: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT status,last_error FROM jobs WHERE job_key=?1",
            [job_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match status {
        None => match external_memory_input(event) {
            ExternalMemoryInput::OverBudget => "over_budget",
            ExternalMemoryInput::NotEligible | ExternalMemoryInput::Eligible(_) => "not_eligible",
        },
        Some((status, _)) if status == "pending" || status == "running" => "queued",
        Some((status, _)) if status == "done" => "completed",
        Some((_, Some(error))) if error.contains("pending queue reached its limit") => {
            "retry_required_queue_full"
        }
        Some(_) => "failed",
    })
}

/// Constructed only by the authenticated HTTP validation boundary.
pub(crate) struct ValidatedEvent(Value);
impl ValidatedEvent {
    pub(crate) fn validate(
        evidence: crate::api::auth::AuthorizedEvidence,
    ) -> Result<Self, &'static str> {
        let value = evidence.value();
        crate::api::external_history::validate(value)?;
        Ok(Self(value.clone()))
    }
}
impl DbStore {
    pub(crate) async fn record_external(
        &self,
        event: ValidatedEvent,
    ) -> Result<Result<Value, &'static str>> {
        self.run(move |c| {
            let e = event.0;
            let text = |key: &str| e[key].as_str().unwrap_or_default();
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let prior: Option<(String,String,String)> = tx.query_row(
                "SELECT content_digest,receipt_id,ingested_at FROM external_history_events WHERE producer_id=?1 AND event_id=?2",
                params![text("producer_id"),text("event_id")], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if let Some((digest,id,at)) = prior {
                if digest != text("content_digest") { return Ok(Err("duplicate_event_id_conflict")); }
                let memory_extraction = extraction_status(&tx, &format!("external:{id}"), &e)?;
                tx.commit()?;
                return Ok(Ok(json!({"receipt_id":id,"ingested_at":at,"state":"committed","replay":true,"memory_extraction":memory_extraction})));
            }
            let id=super::uid(); let at=super::now();
            tx.execute("INSERT INTO external_history_events(producer_id,event_id,project_id,logical_session_id,producer_instance_id,producer_sequence,content_digest,envelope,receipt_id,ingested_at,event_type,occurred_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![text("producer_id"),text("event_id"),text("project_id"),text("logical_session_id"),text("producer_instance_id"),e["producer_sequence"].as_i64(),text("content_digest"),e.to_string(),id,at,text("event_type"),text("occurred_at")])?;
            let job_key = format!("external:{id}");
            let memory_extraction = match external_memory_input(&e) {
                ExternalMemoryInput::NotEligible => "not_eligible",
                ExternalMemoryInput::OverBudget => "over_budget",
                ExternalMemoryInput::Eligible(_) => {
                    let queued: i64 = tx.query_row(
                        "SELECT count(*) FROM jobs WHERE status IN ('pending','running')",
                        [],
                        |r| r.get(0),
                    )?;
                    let (status, attempts, last_error) = if queued >= MAX_PENDING_EXTERNAL_MEMORY_JOBS {
                        ("failed", 0, Some("external memory extraction deferred because the pending queue reached its limit"))
                    } else {
                        ("pending", 0, None)
                    };
                    tx.execute(
                        "INSERT INTO jobs(id,job_key,scope,source_id,payload,status,attempts,available_at,last_error,created_at) VALUES(?1,?2,?3,?4,'[]',?5,?6,?7,?8,?9)",
                        params![super::uid(),job_key,text("project_id"),format!("external:{id}"),status,attempts,chrono::Utc::now().timestamp(),last_error,at],
                    )?;
                    if status == "pending" { "queued" } else { "retry_required_queue_full" }
                }
            };
            tx.commit()?;
            Ok(Ok(json!({"receipt_id":id,"ingested_at":at,"state":"committed","replay":false,"memory_extraction":memory_extraction})))
        }).await
    }
}

// Arrival rowids are durable resume cursors, not producer sequence order.
// The append-only table prevents cursor reuse; identical replays insert no row.
impl DbStore {
    pub(crate) async fn external_sessions(
        &self,
        project: Option<String>,
        producer: Option<String>,
        after: i64,
        limit: i64,
    ) -> Result<Value> {
        self.read(move |c| {
            let mut stmt = c.prepare("SELECT producer_id,project_id,logical_session_id,min(rowid),count(*),max(ingested_at) FROM external_history_events WHERE (?1 IS NULL OR project_id=?1) AND (?2 IS NULL OR producer_id=?2) GROUP BY producer_id,project_id,logical_session_id HAVING min(rowid)>?3 ORDER BY min(rowid) LIMIT ?4")?;
            let mut rows = stmt.query_map(params![project,producer,after,limit+1], |r| Ok(json!({"producer_id":r.get::<_,String>(0)?,"project_id":r.get::<_,String>(1)?,"logical_session_id":r.get::<_,String>(2)?,"cursor":r.get::<_,i64>(3)?,"event_count":r.get::<_,i64>(4)?,"last_ingested_at":r.get::<_,String>(5)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more=rows.len()>limit as usize; rows.truncate(limit as usize);
            let next=rows.last().and_then(|r|r["cursor"].as_i64()).unwrap_or(after);
            Ok(json!({"sessions":rows,"next_cursor":next,"has_more":more}))
        }).await
    }
    pub(crate) async fn external_activity(
        &self,
        project: String,
        producer: String,
        session: String,
        after: i64,
        limit: i64,
    ) -> Result<Value> {
        self.read(move |c| {
            let mut stmt=c.prepare("SELECT rowid,receipt_id,ingested_at,envelope FROM external_history_events WHERE project_id=?1 AND producer_id=?2 AND logical_session_id=?3 AND rowid>?4 ORDER BY rowid LIMIT ?5")?;
            let raw=stmt.query_map(params![project,producer,session,after,limit+1], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more=raw.len()>limit as usize;
            let mut rows=Vec::new();
            for (cursor,receipt,at,text) in raw.into_iter().take(limit as usize) {
                let envelope:Value=serde_json::from_str(&text)?;
                rows.push(json!({"cursor":cursor,"receipt_id":receipt,"ingested_at":at,"state":"committed","producer_acknowledgement":"unknown","envelope":envelope}));
            }
            let next=rows.last().and_then(|r|r["cursor"].as_i64()).unwrap_or(after);
            Ok(json!({"events":rows,"next_cursor":next,"has_more":more,"local_backlog":"unknown","ordering":"arrival cursor; producer sequence only within each instance"}))
        }).await
    }
    pub(crate) async fn external_artifact(
        &self,
        project: String,
        producer: String,
        session: String,
        event: String,
    ) -> Result<Option<Value>> {
        self.read(move |c| {
            let saved:Option<(String,String)>=c.query_row("SELECT receipt_id,envelope FROM external_history_events WHERE project_id=?1 AND producer_id=?2 AND logical_session_id=?3 AND event_id=?4 AND event_type='artifact.recorded'",params![project,producer,session,event], |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            saved.map(|(receipt,text)| {
                let e:Value=serde_json::from_str(&text)?;
                // References confer neither filesystem nor network authority.
                // No byte store exists for external artifacts; never alias internal IDs.
                Ok(json!({"receipt_id":receipt,"envelope":e,"content_available":false,"reason":"Artifact bytes were not ingested; reference metadata only."}))
            }).transpose()
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> ValidatedEvent {
        // Test-only constructor; production requires authenticated evidence.
        ValidatedEvent(
            serde_json::from_str(include_str!(
                "../../tests/external_history/accepted/tool_completed.json"
            ))
            .unwrap(),
        )
    }
    fn message_event(id: &str, text: &str) -> ValidatedEvent {
        let mut value: Value = serde_json::from_str(include_str!(
            "../../tests/external_history/accepted/message_observed_client_supplied.json"
        ))
        .unwrap();
        value["event_id"] = json!(id);
        value["payload"]["text"] = json!(text);
        let mut canonical = value.clone();
        canonical.as_object_mut().unwrap().remove("content_digest");
        value["content_digest"] = json!(crate::safety::fingerprint(&canonical.to_string()));
        ValidatedEvent(value)
    }
    #[tokio::test]
    async fn failed_insert_and_failed_commit_never_acknowledge() {
        let db = DbStore::init(":memory:").unwrap();
        db.run(|c| { c.execute_batch("CREATE TRIGGER fail_external BEFORE INSERT ON external_history_events BEGIN SELECT RAISE(ABORT,'synthetic disk failure'); END;")?; Ok(()) }).await.unwrap();
        assert!(db.record_external(event()).await.is_err());
        db.run(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM external_history_events", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            c.execute_batch("DROP TRIGGER fail_external")?;
            c.commit_hook(Some(|| true));
            Ok(())
        })
        .await
        .unwrap();
        assert!(db.record_external(event()).await.is_err());
        db.run(|c| {
            c.commit_hook(None::<fn() -> bool>);
            assert_eq!(
                c.query_row("SELECT count(*) FROM external_history_events", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
        assert!(db.record_external(event()).await.unwrap().is_ok());
    }
    #[tokio::test]
    async fn client_message_outbox_is_replay_safe_review_only_and_deduplicates_evidence() {
        let db = DbStore::init(":memory:").unwrap();
        let text = "Use SQLite for the Harness database.";
        let first = db
            .record_external(message_event("memory-1", text))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first["memory_extraction"], "queued");
        let replay = db
            .record_external(message_event("memory-1", text))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replay["memory_extraction"], "queued");
        assert_eq!(replay["receipt_id"], first["receipt_id"]);
        db.run(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM jobs", [], |r| r.get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                c.query_row("SELECT payload FROM jobs", [], |r| r.get::<_, String>(0))?,
                "[]"
            );
            Ok(())
        })
        .await
        .unwrap();
        let job = db.claim_job().await.unwrap().unwrap();
        assert_eq!(job.scope, "proj-harness");
        assert_eq!(job.events.len(), 1);
        assert_eq!(job.events[0].role, "external_user");
        assert_eq!(job.events[0].content, text);
        let proposal = |evidence_id: &str| crate::storage::Proposal {
            key: "database_choice".into(),
            value: "SQLite".into(),
            category: "decision".into(),
            evidence_id: evidence_id.into(),
            quote: text.into(),
            priority: Some("high".into()),
        };
        db.finish_job(job, vec![proposal("memory-1")])
            .await
            .unwrap();
        let feed = db
            .candidate_feed("proj-harness".into(), None, true, false)
            .await
            .unwrap();
        let candidate = feed["candidates"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(feed["candidates"][0]["priority"], "high");
        assert_eq!(
            feed["candidates"][0]["evidence"]["origin"],
            "external-history"
        );
        assert_eq!(
            feed["candidates"][0]["evidence"]["receipt_id"],
            first["receipt_id"]
        );
        assert!(
            db.recall("proj-harness".into(), "database".into())
                .await
                .unwrap()
                .is_empty(),
            "pending evidence must not enter active recall"
        );
        assert_eq!(
            db.resolve(candidate.clone(), "proj-harness".into(), true)
                .await
                .unwrap(),
            "approved"
        );
        assert!(db
            .recall("proj-harness".into(), "database".into())
            .await
            .unwrap()
            .iter()
            .any(|m| m.value == "SQLite"));
        db.record_external(message_event("memory-2", text))
            .await
            .unwrap()
            .unwrap();
        let second = db.claim_job().await.unwrap().unwrap();
        db.finish_job(second, vec![proposal("memory-2")])
            .await
            .unwrap();
        db.run(move|c|{
            let evidence:String=c.query_row("SELECT evidence FROM candidates WHERE id=?1",[candidate],|r|r.get(0))?;
            let evidence:Value=serde_json::from_str(&evidence)?;
            assert_eq!(evidence["supporting_evidence"].as_array().unwrap().len(),2);
            assert_eq!(c.query_row("SELECT count(*) FROM memories WHERE scope='proj-harness' AND key='database_choice'",[],|r|r.get::<_,i64>(0))?,1);
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn external_tool_payload_is_history_only_and_budgets_leave_receipts_durable() {
        let db = DbStore::init(":memory:").unwrap();
        let tool = db.record_external(event()).await.unwrap().unwrap();
        assert_eq!(tool["memory_extraction"], "not_eligible");
        let large = "x".repeat(MAX_EXTERNAL_MEMORY_TEXT_BYTES + 1);
        let oversized = db
            .record_external(message_event("too-large", &large))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(oversized["memory_extraction"], "over_budget");
        let replay = db
            .record_external(message_event("too-large", &large))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replay["memory_extraction"], "over_budget");
        db.run(|c|{
            assert_eq!(c.query_row("SELECT count(*) FROM external_history_events",[],|r|r.get::<_,i64>(0))?,2);
            assert_eq!(c.query_row("SELECT count(*) FROM jobs",[],|r|r.get::<_,i64>(0))?,0);
            for i in 0..MAX_PENDING_EXTERNAL_MEMORY_JOBS {
                let id=format!("seed-{i}");
                c.execute("INSERT INTO jobs(id,job_key,scope,source_id,payload,status,attempts,available_at,created_at) VALUES(?1,?2,'global','seed','[]','pending',0,0,'2000-01-01')",params![id,id])?;
            }
            Ok(())
        }).await.unwrap();
        let queued = db
            .record_external(message_event("queue-full", "Use SQLite."))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(queued["memory_extraction"], "retry_required_queue_full");
        db.run(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM external_history_events", [], |r| r
                    .get::<_, i64>(0))?,
                3
            );
            assert_eq!(
                c.query_row(
                    "SELECT status FROM jobs WHERE source_id LIKE 'external:%'",
                    [],
                    |r| r.get::<_, String>(0)
                )?,
                "failed"
            );
            Ok(())
        })
        .await
        .unwrap();
        let job_id = db
            .run(|c| {
                let id: String = c.query_row(
                    "SELECT id FROM jobs WHERE source_id LIKE 'external:%'",
                    [],
                    |r| r.get(0),
                )?;
                c.execute("DELETE FROM jobs WHERE source_id='seed'", [])?;
                Ok(id)
            })
            .await
            .unwrap();
        assert!(db.retry_job(job_id).await.unwrap());
        let replay = db
            .record_external(message_event("queue-full", "Use SQLite."))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replay["memory_extraction"], "queued");
        assert_eq!(replay["replay"], true);
    }

    #[tokio::test]
    async fn concurrent_replays_converge_and_conflicts_do_not_mutate() {
        let db = DbStore::init(":memory:").unwrap();
        let (a, b) = tokio::join!(db.record_external(event()), db.record_external(event()));
        let (a, b) = (a.unwrap().unwrap(), b.unwrap().unwrap());
        assert_eq!(a["receipt_id"], b["receipt_id"]);
        assert_ne!(a["replay"], b["replay"]);
        let mut changed = event();
        changed.0["content_digest"] = json!("a".repeat(64));
        assert_eq!(
            db.record_external(changed).await.unwrap().unwrap_err(),
            "duplicate_event_id_conflict"
        );
        db.run(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM external_history_events", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    }
}
