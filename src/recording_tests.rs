//! Native release gates. These must run with cargo test, not just the SQL mirrors.
use crate::{
    recording::{self, Admission, CaptureInput},
    storage::DbStore,
};
use serde_json::json;
fn input(request: &str, session: &str) -> CaptureInput {
    CaptureInput {
        request: request.into(),
        session: session.into(),
        scope: "global".into(),
        prompt: "I prefer Rust".into(),
        model: "synthetic".into(),
        signature: format!("signature:{session}"),
        redacted: false,
    }
}
async fn start(db: &DbStore, request: &str, session: &str) {
    assert!(matches!(
        db.capture_chat(input(request, session)).await.unwrap(),
        Admission::Saved(_)
    ));
    assert_eq!(
        db.claim_recording().await.unwrap().unwrap().request,
        request
    );
    db.save_recording_context(
        request.into(),
        json!({"memories":[],"provider_messages":[]}),
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn admission_is_idempotent_and_content_bound() {
    let db = DbStore::init(":memory:").unwrap();
    for _ in 0..2 {
        assert!(matches!(
            db.capture_chat(input("r", "s")).await.unwrap(),
            Admission::Saved(_)
        ));
    }
    let mut changed = input("r", "s");
    changed.signature = "different".into();
    assert!(matches!(
        db.capture_chat(changed).await.unwrap(),
        Admission::Conflict
    ));
    assert_eq!(
        db.history("s".into(), None).await.unwrap()["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[tokio::test]
async fn unfinished_turn_blocks_only_its_own_session() {
    let db = DbStore::init(":memory:").unwrap();
    db.capture_chat(input("a", "s")).await.unwrap();
    assert!(matches!(
        db.capture_chat(input("b", "s")).await.unwrap(),
        Admission::Busy
    ));
    assert!(matches!(
        db.capture_chat(input("c", "other")).await.unwrap(),
        Admission::Saved(_)
    ));
}
#[tokio::test]
async fn full_memory_queue_does_not_rollback_answer() {
    let db = DbStore::init(":memory:").unwrap();
    start(&db, "r", "s").await;
    db.run(|c|{let tx=c.transaction()?;for i in 0..1000 {tx.execute("INSERT INTO jobs(id,job_key,scope,source_id,payload,status,available_at,created_at) VALUES(?1,?1,'global','fixture','[]','pending',0,'now')",[format!("bulk-{i}")])?;}tx.commit()?;Ok(())}).await.unwrap();
    db.complete_recording("r".into(), "Answer saved".into())
        .await
        .unwrap();
    assert_eq!(db.flush_recording_outbox().await.unwrap(), 0);
    let receipt = db.recording_receipt("r".into()).await.unwrap().unwrap();
    assert_eq!(receipt["state"], "complete");
    assert_eq!(receipt["memory_status"], "deferred");
    assert_eq!(receipt["response"], "Answer saved");
    db.run(|c| {
        c.execute("UPDATE jobs SET status='done' WHERE id='bulk-0'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(db.flush_recording_outbox().await.unwrap(), 1);
    assert_eq!(db.flush_recording_outbox().await.unwrap(), 0);
}
#[tokio::test]
async fn streaming_completion_and_receipt_commit_together() {
    let db = DbStore::init(":memory:").unwrap();
    start(&db, "r", "s").await;
    db.complete_recording("r".into(), "answer".into()).await.unwrap();
    let events = db.generation_since("s".into(), 0).await.unwrap();
    assert_eq!(events["events"][0]["content"], "answer");
    assert_eq!(events["events"][1]["state"], "completed");
    assert_eq!(events["events"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn streaming_event_failure_rolls_back_answer_and_receipt() {
    let db = DbStore::init(":memory:").unwrap();
    start(&db, "r", "s").await;
    db.run(|c| {
        c.execute_batch("CREATE TRIGGER reject_generation BEFORE INSERT ON generation_events BEGIN SELECT RAISE(ABORT, 'injected failure'); END;")?;
        Ok(())
    }).await.unwrap();
    assert!(db.complete_recording("r".into(), "answer".into()).await.is_err());
    assert_eq!(db.recording_receipt("r".into()).await.unwrap().unwrap()["state"], "generating");
    assert_eq!(db.history("s".into(), None).await.unwrap()["messages"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn context_failure_never_completes_answer() {
    let db = DbStore::init(":memory:").unwrap();
    db.capture_chat(input("r", "s")).await.unwrap();
    db.claim_recording().await.unwrap();
    assert!(db
        .complete_recording("r".into(), "Not persisted".into())
        .await
        .is_err());
    assert_eq!(
        db.history("s".into(), None).await.unwrap()["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[tokio::test]
async fn context_receipt_is_immutable_and_available_after_failure() {
    let db = DbStore::init(":memory:").unwrap();
    start(&db, "r", "s").await;
    assert!(db
        .save_recording_context("r".into(), json!({"changed":true}))
        .await
        .is_err());
    db.fail_recording("r".into(), "provider_failed")
        .await
        .unwrap();
    let detail = db.recording_context("r".into()).await.unwrap().unwrap();
    assert_eq!(detail["state"], "failed");
    assert_eq!(detail["context"]["memories"], json!([]));
    assert_eq!(db.flush_recording_outbox().await.unwrap(), 1);
    assert!(db.claim_recording().await.unwrap().is_none());
}
#[tokio::test]
async fn restart_preserves_waiting_and_marks_started_as_interrupted() {
    let db = DbStore::init(":memory:").unwrap();
    start(&db, "r", "s").await;
    db.capture_chat(input("waiting", "other")).await.unwrap();
    db.run(recording::recover).await.unwrap();
    db.run(recording::recover).await.unwrap();
    assert_eq!(
        db.recording_receipt("r".into()).await.unwrap().unwrap()["state"],
        "interrupted"
    );
    assert_eq!(
        db.recording_receipt("waiting".into())
            .await
            .unwrap()
            .unwrap()["state"],
        "captured"
    );
    let context = db.recording_context("r".into()).await.unwrap().unwrap();
    assert_eq!(
        context["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["kind"] == "interrupted")
            .count(),
        1
    );
}
#[tokio::test]
async fn history_replays_complete_pairs_not_failed_prompts() {
    let db = DbStore::init(":memory:").unwrap();
    start(&db, "a", "s").await;
    db.complete_recording("a".into(), "A".into()).await.unwrap();
    start(&db, "b", "s").await;
    db.fail_recording("b".into(), "provider_failed")
        .await
        .unwrap();
    db.capture_chat(input("c", "s")).await.unwrap();
    let next = db.claim_recording().await.unwrap().unwrap();
    assert_eq!(
        next.events
            .iter()
            .map(|e| e.role.as_str())
            .collect::<Vec<_>>(),
        vec!["user", "assistant", "user"]
    );
    assert_eq!(next.events[0].id, "a");
    assert_eq!(next.events[2].id, "c");
}
#[tokio::test]
async fn actual_history_and_sessions_queries_page_older_rows() {
    let db = DbStore::init(":memory:").unwrap();
    db.run(|c| {let tx=c.transaction()?;tx.execute("INSERT INTO sessions VALUES('s','global','now')",[])?;for i in 0..257 {tx.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?1,'s','user','fixture','complete','now')",[i.to_string()])?;}tx.commit()?;Ok(())}).await.unwrap();
    let mut cursor = None;
    let mut seen = std::collections::HashSet::new();
    loop {
        let page = db.history("s".into(), cursor).await.unwrap();
        for m in page["messages"].as_array().unwrap() {
            assert!(seen.insert(m["id"].as_str().unwrap().to_string()));
        }
        cursor = page["next_before_seq"].as_i64();
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen.len(), 257);
    assert_eq!(
        db.recorded_sessions(None).await.unwrap()["sessions"][0]["message_count"],
        257
    );
}
