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
    db.complete_recording("r".into(), "answer".into())
        .await
        .unwrap();
    let events = db.generation_since("s".into(), 0).await.unwrap();
    assert!(events["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["request_id"] == "r"));
    // P7-T05: completion writes the terminal row only. Answer chunks are published
    // incrementally by the sink, so no duplicate content row is written here.
    assert_eq!(events["events"][0]["state"], "completed");
    assert_eq!(events["events"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn generation_replay_attributes_multiple_turns_and_resumes_by_cursor() {
    let db = DbStore::init(":memory:").unwrap();
    start(&db, "first", "session").await;
    db.complete_recording("first".into(), "one".into())
        .await
        .unwrap();
    start(&db, "second", "session").await;
    db.complete_recording("second".into(), "two".into())
        .await
        .unwrap();

    let feed = db.generation_since("session".into(), 0).await.unwrap();
    let events = feed["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events
            .iter()
            .map(|event| event["request_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    let first_cursor = events[0]["seq"].as_i64().unwrap();

    let tail = db
        .generation_since("session".into(), first_cursor)
        .await
        .unwrap();
    assert_eq!(tail["events"], json!([events[1].clone()]));
    assert_eq!(tail["next_after_seq"], events[1]["seq"]);
    assert!(
        db.generation_since("other".into(), 0).await.unwrap()["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn streaming_event_failure_rolls_back_answer_and_receipt() {
    let db = DbStore::init(":memory:").unwrap();
    start(&db, "r", "s").await;
    db.run(|c| {
        c.execute_batch("CREATE TRIGGER reject_generation BEFORE INSERT ON generation_events BEGIN SELECT RAISE(ABORT, 'injected failure'); END;")?;
        Ok(())
    }).await.unwrap();
    assert!(db
        .complete_recording("r".into(), "answer".into())
        .await
        .is_err());
    assert_eq!(
        db.recording_receipt("r".into()).await.unwrap().unwrap()["state"],
        "generating"
    );
    assert_eq!(
        db.history("s".into(), None).await.unwrap()["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
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
    let first_generation = db.generation_since("s".into(), 0).await.unwrap();
    assert_eq!(first_generation["events"].as_array().unwrap().len(), 1);
    assert_eq!(first_generation["events"][0]["state"], "interrupted");
    assert_eq!(first_generation["events"][0]["request_id"], "r");
    db.run(recording::recover).await.unwrap();
    assert_eq!(
        db.generation_since("s".into(), 0).await.unwrap(),
        first_generation
    );
    assert!(
        db.generation_since("other".into(), 0).await.unwrap()["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
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
async fn history_exposes_failure_code_for_truthful_terminal_ui() {
    let db = DbStore::init(":memory:").unwrap();
    start(&db, "failed-context", "s").await;
    db.fail_recording("failed-context".into(), "context_failed")
        .await
        .unwrap();
    let history = db.history("s".into(), None).await.unwrap();
    let messages = history["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["generation_state"], "failed");
    assert_eq!(messages[0]["error_code"], "context_failed");
}

#[tokio::test]
async fn actual_history_and_sessions_queries_page_older_rows() {
    let db = DbStore::init(":memory:").unwrap();
    db.run(|c| {let tx=c.transaction()?;tx.execute("INSERT INTO sessions(id,scope,created_at) VALUES('s','global','now')",[])?;for i in 0..257 {tx.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?1,'s','user','fixture','complete','now')",[i.to_string()])?;}tx.commit()?;Ok(())}).await.unwrap();
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

/// P15-T02: the retrieval explanation is written as evidence, so it must survive a read exactly
/// as recall produced it, refuse to be rewritten, and carry a fingerprint rather than the prompt.
#[tokio::test]
async fn context_retrieval_receipt_is_write_once_and_records_why() {
    let db = DbStore::init(":memory:").unwrap();
    start(&db, "r", "s").await;
    let candidate = db
        .save_compaction_candidate(
            "global".into(),
            "req".into(),
            "step".into(),
            "The operator prefers Rust for systems work".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        db.resolve(candidate, "global".into(), true).await.unwrap(),
        "approved"
    );
    let (recalled, candidates) = db
        .recall_explained(
            "global".into(),
            "Rust systems".into(),
            crate::embeddings::Strategy::Hybrid,
        )
        .await
        .unwrap();
    assert_eq!(recalled.len(), 1);
    assert_eq!(candidates.len(), 1);
    assert!(db
        .save_retrieval_receipt(
            "r".into(),
            "global".into(),
            crate::embeddings::Strategy::Hybrid,
            "Rust systems".into(),
            6_144,
            candidates.clone()
        )
        .await
        .unwrap());
    // A turn's evidence is written once. A retry must not be able to rewrite history.
    assert!(!db
        .save_retrieval_receipt(
            "r".into(),
            "global".into(),
            crate::embeddings::Strategy::LexicalOnly,
            "a different prompt".into(),
            1,
            Vec::new()
        )
        .await
        .unwrap());
    let receipt = db.retrieval_receipt("r".into()).await.unwrap().unwrap();
    assert_eq!(receipt["strategy"], "hybrid");
    assert_eq!(receipt["embedding_model"], crate::embeddings::MODEL);
    assert_eq!(receipt["considered"], 1);
    assert_eq!(receipt["included"], 1);
    assert_eq!(receipt["budget_bytes"], 6_144);
    let row = &receipt["candidates"][0];
    assert_eq!(row["decision"], "included");
    assert_eq!(row["reason"], "ranked_and_fit");
    assert_eq!(row["revision"], 1);
    assert!(row["total_score"].as_f64().unwrap() > 0.0);
    assert_eq!(row["bytes"], receipt["included_bytes"]);
    // The prompt itself is not stored, only a fingerprint of it.
    assert!(!receipt["prompt_fingerprint"]
        .as_str()
        .unwrap()
        .contains("Rust"));
    assert!(db
        .retrieval_receipt("missing".into())
        .await
        .unwrap()
        .is_none());
}

/// P15-T02: a rehearsal must answer with the shipped ranking and still change nothing. If the
/// preview could commit, it would be an approval wearing a preview's name.
#[tokio::test]
async fn context_retrieval_preview_rehearses_without_writing() {
    let db = DbStore::init(":memory:").unwrap();
    let candidate = db
        .save_compaction_candidate(
            "global".into(),
            "req".into(),
            "step".into(),
            "The operator prefers Rust for systems work".into(),
        )
        .await
        .unwrap();
    let recall_now = || async {
        db.recall_explained(
            "global".into(),
            "Rust systems".into(),
            crate::embeddings::Strategy::Hybrid,
        )
        .await
        .unwrap()
        .0
        .len()
    };
    assert_eq!(recall_now().await, 0);

    let preview = db
        .preview_retrieval(
            "global".into(),
            "Rust systems".into(),
            Some(candidate.clone()),
        )
        .await
        .unwrap();
    assert_eq!(preview["candidate_state"], "approved");
    assert_eq!(preview["rehearsed"], true);
    assert!(preview["before"].as_array().unwrap().is_empty());
    assert_eq!(preview["added"].as_array().unwrap().len(), 1);
    assert!(preview["removed"].as_array().unwrap().is_empty());
    assert_eq!(preview["after"][0]["decision"], "included");
    assert!(preview["note"]
        .as_str()
        .unwrap()
        .contains("not how the model"));

    // Rolled back: the candidate is still pending and recall still returns nothing.
    let feed = db
        .candidate_feed("global".into(), None, false, false)
        .await
        .unwrap();
    assert_eq!(feed["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(feed["candidates"][0]["id"], candidate);
    assert_eq!(recall_now().await, 0);

    let plain = db
        .preview_retrieval("global".into(), "Rust systems".into(), None)
        .await
        .unwrap();
    assert_eq!(plain["candidate_state"], "none");
    assert_eq!(plain["rehearsed"], false);
    assert!(plain["added"].as_array().unwrap().is_empty());
}

/// P15-T03: a review branch must shadow 'main' for the same key without overwriting it, and a
/// lapsed temporary memory must stop being recalled while its value is retained.
#[tokio::test]
async fn governance_branches_and_expiry_gate_recall_without_deleting_history() {
    let db = DbStore::init(":memory:").unwrap();
    let reviewed = db
        .save_compaction_candidate(
            "global".into(),
            "req".into(),
            "step".into(),
            "The operator prefers Rust for systems work".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        db.resolve(reviewed, "global".into(), true).await.unwrap(),
        "approved"
    );
    let recall_values = || async {
        db.recall_explained(
            "global".into(),
            "systems work".into(),
            crate::embeddings::Strategy::Hybrid,
        )
        .await
        .unwrap()
        .0
        .into_iter()
        .map(|m| m.value)
        .collect::<Vec<_>>()
    };
    assert_eq!(recall_values().await.len(), 1);

    let checkout = db
        .checkout_memory_branch(
            "global".into(),
            "review".into(),
            Some("trialling a revision".into()),
            true,
        )
        .await
        .unwrap();
    assert_eq!(checkout["active_branch"], "review");
    let trial = db
        .save_compaction_candidate(
            "global".into(),
            "req2".into(),
            "step2".into(),
            "The operator prefers Go for systems work".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        db.resolve(trial, "global".into(), true).await.unwrap(),
        "approved"
    );
    // Both values are stored, but only the branch value is recalled while it is checked out.
    let recalled = recall_values().await;
    assert_eq!(recalled.len(), 1);
    assert!(recalled[0].contains("Go"));

    let overview = db.memory_governance("global".into()).await.unwrap();
    assert_eq!(overview["active_branch"], "review");
    assert_eq!(overview["entries"].as_array().unwrap().len(), 2);
    let branch_entry = overview["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["branch"] == "review")
        .unwrap()
        .clone();
    let branch_id = branch_entry["id"].as_str().unwrap().to_string();

    // A temporary memory whose clock has passed stops being recalled, and the sweep records why.
    assert_eq!(
        db.govern_memory(
            "global".into(),
            branch_id.clone(),
            "expire".into(),
            Some(chrono::Utc::now().timestamp() - 10),
            None,
            None,
            None
        )
        .await
        .unwrap(),
        "expiry_scheduled"
    );
    let lapsed = recall_values().await;
    assert_eq!(lapsed.len(), 1);
    assert!(lapsed[0].contains("Rust"));
    assert_eq!(db.lapse_expired_memories().await.unwrap(), 1);
    assert_eq!(db.lapse_expired_memories().await.unwrap(), 0);
    let timeline = db
        .memory_timeline("global".into(), branch_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(timeline["status"], "expired");
    assert_eq!(
        timeline["value"],
        "The operator prefers Go for systems work"
    );
    assert!(timeline["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["action"] == "expire" && row["detail"] == "lapsed"));
    assert!(timeline["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["reason"] == "expired"));
}

/// P15-T03: pinning, duplicate merges and usefulness verdicts are owner acts, so each one has to
/// leave an audited trail and a rejected value has to stay on the timeline as considered.
#[tokio::test]
async fn governance_pins_merges_and_feedback_are_recorded_as_history() {
    let db = DbStore::init(":memory:").unwrap();
    let kept = db
        .save_compaction_candidate(
            "global".into(),
            "req".into(),
            "step".into(),
            "The operator prefers Rust for systems work".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        db.resolve(kept, "global".into(), true).await.unwrap(),
        "approved"
    );
    let turned_down = db
        .save_compaction_candidate(
            "global".into(),
            "req2".into(),
            "step2".into(),
            "The operator prefers Perl for systems work".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        db.resolve(turned_down, "global".into(), false)
            .await
            .unwrap(),
        "rejected"
    );
    // A duplicate of the kept value, sharing its dedup key, so the overview can suggest a merge.
    db.run(|c| {
        c.execute("INSERT INTO memories(id,scope,key,value,branch,category,status,revision,candidate_id,created_at,updated_at) SELECT 'duplicate-row',scope,'turn_summary_copy',value,branch,category,'active',1,candidate_id,created_at,updated_at FROM memories WHERE key='turn_summary'",[])?;
        Ok(())
    })
    .await
    .unwrap();

    let overview = db.memory_governance("global".into()).await.unwrap();
    assert_eq!(
        overview["duplicate_suggestions"].as_array().unwrap().len(),
        1
    );
    assert_eq!(overview["duplicate_suggestions"][0]["count"], 2);
    let keeper = overview["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == "turn_summary")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    assert_eq!(
        db.govern_memory(
            "global".into(),
            keeper.clone(),
            "merge".into(),
            None,
            Some("duplicate-row".into()),
            None,
            None
        )
        .await
        .unwrap(),
        "merged"
    );
    assert_eq!(
        db.govern_memory(
            "global".into(),
            keeper.clone(),
            "pin".into(),
            None,
            None,
            Some("owner asked for this to always be present".into()),
            None
        )
        .await
        .unwrap(),
        "pin"
    );
    assert_eq!(
        db.govern_memory(
            "global".into(),
            keeper.clone(),
            "useful".into(),
            None,
            None,
            None,
            None
        )
        .await
        .unwrap(),
        "recorded"
    );
    // The same verdict for the same revision is not counted twice.
    assert_eq!(
        db.govern_memory(
            "global".into(),
            keeper.clone(),
            "useful".into(),
            None,
            None,
            None,
            None
        )
        .await
        .unwrap(),
        "duplicate_feedback"
    );
    assert_eq!(
        db.govern_memory(
            "global".into(),
            "missing".into(),
            "pin".into(),
            None,
            None,
            None,
            None
        )
        .await
        .unwrap(),
        "not_found"
    );

    let after = db.memory_governance("global".into()).await.unwrap();
    assert_eq!(after["pinned"].as_array().unwrap().len(), 1);
    assert!(after["duplicate_suggestions"]
        .as_array()
        .unwrap()
        .is_empty());
    let absorbed = after["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "duplicate-row")
        .unwrap()
        .clone();
    // The absorbed row is retained as superseded rather than deleted.
    assert_eq!(absorbed["status"], "superseded");

    let timeline = db
        .memory_timeline("global".into(), keeper)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(timeline["pinned"], true);
    let actions = timeline["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["action"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(actions.contains(&"approve".to_string()));
    assert!(actions.contains(&"merge".to_string()));
    assert!(actions.contains(&"pin".to_string()));
    assert_eq!(timeline["feedback"].as_array().unwrap().len(), 1);
    assert_eq!(timeline["feedback"][0]["verdict"], "useful");
    let decisions = timeline["decisions"].as_array().unwrap();
    assert!(decisions
        .iter()
        .any(|row| row["state"] == "chosen" && row["reason"] == "approved"));
    assert!(decisions
        .iter()
        .any(|row| row["state"] == "considered" && row["reason"] == "rejected"));
    assert!(decisions
        .iter()
        .any(|row| row["reason"] == "duplicate_merged"));
}
