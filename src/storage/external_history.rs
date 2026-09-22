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
