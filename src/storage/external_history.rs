//! Commit evidence and receipt atomically. No extraction/provider/tool dispatch.
use super::DbStore;
use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

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
                tx.commit()?;
                return Ok(Ok(json!({"receipt_id":id,"ingested_at":at,"state":"committed","replay":true})));
            }
            let id=super::uid(); let at=super::now();
            tx.execute("INSERT INTO external_history_events(producer_id,event_id,project_id,logical_session_id,producer_instance_id,producer_sequence,content_digest,envelope,receipt_id,ingested_at,event_type,occurred_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![text("producer_id"),text("event_id"),text("project_id"),text("logical_session_id"),text("producer_instance_id"),e["producer_sequence"].as_i64(),text("content_digest"),e.to_string(),id,at,text("event_type"),text("occurred_at")])?;
            tx.commit()?;
            Ok(Ok(json!({"receipt_id":id,"ingested_at":at,"state":"committed","replay":false})))
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
