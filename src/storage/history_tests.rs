use super::*;
use crate::storage::DbStore;

const SESSION: &str = "11111111-1111-4111-8111-111111111111";
const TURN: &str = "22222222-2222-4222-8222-222222222222";
const SECRET_TURN: &str = "33333333-3333-4333-8333-333333333333";
const CANDIDATE: &str = "44444444-4444-4444-8444-444444444444";
const MEMORY: &str = "55555555-5555-4555-8555-555555555555";

/// One session with two complete turns — one ordinary, one carrying a secret — plus one
/// artifact and one approved memory. Enough to prove scoping, the sanitizer gate, the
/// forget/delete difference and an export round trip against real rows.
async fn seed(db: &DbStore) {
    db.run(|c| {
            let stamp = "2026-09-15T00:00:00+00:00";
            c.execute(
                "INSERT INTO sessions(id,scope,created_at) VALUES(?1,'proj',?2)",
                params![SESSION, stamp],
            )?;
            c.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?1,?2,'user','we chose SQLite for durability','complete',?3)",
                params![TURN, SESSION, stamp])?;
            c.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?1,?2,'user','the deploy api_key is sk-abcdefghijklmnop','complete',?3)",
                params![SECRET_TURN, SESSION, stamp])?;
            c.execute("INSERT INTO sources(id,scope,name,format,fingerprint,parser_version,content,warnings,created_at) VALUES('artifact-1','proj','design notes','jsonl','fp','1','the durability plan uses WAL','[]',?1)",
                [stamp])?;
            c.execute("INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at) VALUES(?1,'proj','language','Rust systems','preference','artifact-1','{}',0,'approved',?2,2000000000)",
                params![CANDIDATE, stamp])?;
            c.execute("INSERT INTO memories(id,scope,key,value,category,status,revision,candidate_id,created_at,updated_at) VALUES(?1,'proj','language','Rust systems','preference','active',1,?2,?3,?3)",
                params![MEMORY, CANDIDATE, stamp])?;
            Ok(json!({}))
        })
        .await
        .unwrap();
}

fn scope_of(scope: &str) -> SearchScope {
    SearchScope {
        scope: scope.into(),
        session_id: None,
        kind: None,
    }
}

/// Clause 1. Search is scoped, every hit carries a traceable citation, and content the
/// shared sanitizer refuses is never indexed and therefore never returned.
#[tokio::test]
async fn scoped_search_returns_citations_and_never_leaks_unsanitized_content() {
    let db = DbStore::init(":memory:").unwrap();
    seed(&db).await;
    let report = db.index_history(100).await.unwrap();
    // Two of the three sources are indexable; the secret turn is refused by name.
    assert_eq!(report["indexed"], 2, "{report}");
    let refused = report["refused"].as_array().unwrap();
    assert_eq!(refused.len(), 1, "{report}");
    assert_eq!(refused[0]["source_id"], SECRET_TURN);
    assert_eq!(refused[0]["reason"], "sensitive_content");

    let hits = db
        .search_history(scope_of("proj"), "durability".into())
        .await
        .unwrap();
    assert_eq!(hits["returned"], 2, "{hits}");
    for result in hits["results"].as_array().unwrap() {
        let citation = &result["citation"];
        // A citation identifies the source row, its revision, its kind and its timestamp.
        assert!(citation["id"].is_string());
        assert!(citation["source_id"].is_string());
        assert!(matches!(
            citation["kind"].as_str().unwrap(),
            "turn" | "artifact"
        ));
        assert_eq!(citation["revision"], 1);
        assert_eq!(citation["scope"], "proj");
        assert_eq!(citation["sanitizer"], review::SANITIZER);
        assert!(citation["timestamp"].as_str().unwrap().starts_with("2026-"));
        assert_eq!(
            citation["content_sha256"],
            review::checksum(result["snippet"].as_str().unwrap()),
            "the citation checksum must describe the text that was returned"
        );
    }

    // The secret is unreachable through search by any of its own words.
    for term in ["api_key", "sk-abcdefghijklmnop", "deploy"] {
        let leak = db
            .search_history(scope_of("proj"), term.into())
            .await
            .unwrap();
        assert_eq!(leak["returned"], 0, "search leaked {term}: {leak}");
    }
    let all = db
        .search_history(scope_of("proj"), "sqlite durability wal".into())
        .await
        .unwrap();
    let text = all.to_string();
    assert!(!text.contains("sk-abcdefghijklmnop"), "{text}");

    // Scoping: another project sees nothing, and a session filter narrows to turns.
    let other = db
        .search_history(scope_of("other"), "durability".into())
        .await
        .unwrap();
    assert_eq!(other["returned"], 0, "{other}");
    let scoped = db
        .search_history(
            SearchScope {
                scope: "proj".into(),
                session_id: Some(SESSION.into()),
                kind: Some("turn".into()),
            },
            "durability".into(),
        )
        .await
        .unwrap();
    assert_eq!(scoped["returned"], 1, "{scoped}");
    assert_eq!(scoped["results"][0]["citation"]["kind"], "turn");

    // Re-indexing unchanged content writes nothing, so an idle sweep is observably idle.
    let again = db.index_history(100).await.unwrap();
    assert_eq!(again["indexed"], 0);
    assert_eq!(again["revision_advanced"], 0);
    assert_eq!(again["unchanged"], 2);
}

/// A stored body that no longer passes the shared sanitizer is suppressed at read time
/// rather than served. This is the gate that makes the index safe even if a row was written
/// by an older or buggier writer.
#[tokio::test]
async fn a_tampered_index_row_is_suppressed_instead_of_returned() {
    let db = DbStore::init(":memory:").unwrap();
    seed(&db).await;
    db.index_history(100).await.unwrap();
    // Simulate an out-of-band write: the row now holds a secret the writer would have refused.
    db.run(|c| {
        c.execute(
            "UPDATE history_documents SET body='durability bearer abc123def456' WHERE kind='turn'",
            [],
        )?;
        Ok(json!({}))
    })
    .await
    .unwrap();
    let hits = db
        .search_history(scope_of("proj"), "durability".into())
        .await
        .unwrap();
    assert_eq!(hits["suppressed"], 1, "{hits}");
    assert!(!hits.to_string().contains("bearer abc123def456"));
}

/// Clause 2. forget and source-delete are different operations with different observable
/// outcomes, and both leave an audit trail.
#[tokio::test]
async fn forget_and_source_delete_differ_observably() {
    let db = DbStore::init(":memory:").unwrap();
    seed(&db).await;
    db.index_history(100).await.unwrap();
    let ids = db
        .read(|c| {
            let mut stmt = c.prepare("SELECT kind,id FROM history_documents ORDER BY kind")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap();
    let artifact = ids.iter().find(|(k, _)| k == "artifact").unwrap().1.clone();
    let turn = ids.iter().find(|(k, _)| k == "turn").unwrap().1.clone();

    // --- forget: stops being returned, everything else survives.
    assert_eq!(
        db.forget_history_document(turn.clone(), false)
            .await
            .unwrap(),
        "forgotten"
    );
    let hits = db
        .search_history(scope_of("proj"), "sqlite".into())
        .await
        .unwrap();
    assert_eq!(
        hits["returned"], 0,
        "a forgotten entry must not be returned"
    );
    let audit = db
        .history_document_audit(turn.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(audit["searchable"], false);
    assert_eq!(
        audit["content_present"], true,
        "forget must not destroy content"
    );
    assert!(audit["forgotten_at"].is_string());
    assert!(audit["source_deleted_at"].is_null());
    let actions: Vec<&str> = audit["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, vec!["index", "forget"]);
    // The source row still holds its text: forgetting is not a deletion.
    let source_text: String = db
        .read({
            let turn_source = TURN.to_string();
            move |c| {
                Ok(c.query_row(
                    "SELECT content FROM messages WHERE id=?1",
                    [&turn_source],
                    |r| r.get(0),
                )?)
            }
        })
        .await
        .unwrap();
    assert_eq!(source_text, "we chose SQLite for durability");
    // Forgetting twice writes nothing twice; restore lifts it because nothing was destroyed.
    assert_eq!(
        db.forget_history_document(turn.clone(), false)
            .await
            .unwrap(),
        "already_forgotten"
    );
    assert_eq!(
        db.forget_history_document(turn.clone(), true)
            .await
            .unwrap(),
        "restored"
    );
    assert_eq!(
        db.search_history(scope_of("proj"), "sqlite".into())
            .await
            .unwrap()["returned"],
        1
    );

    // --- source delete: the content is gone, the record of it having existed is not.
    let outcome = db.delete_history_source(artifact.clone()).await.unwrap();
    assert_eq!(
        outcome, "content_removed_source_retained",
        "an artifact still referenced by a candidate keeps its row, emptied"
    );
    let audit = db
        .history_document_audit(artifact.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(audit["content_present"], false, "content must be gone");
    assert!(audit["source_deleted_at"].is_string());
    assert_eq!(
        audit["citation"]["id"], artifact,
        "the citation must survive so the deletion is provable"
    );
    let actions: Vec<&str> = audit["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, vec!["index", "delete_source"]);
    let source_body: String = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT content FROM sources WHERE id='artifact-1'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        source_body, "",
        "the underlying source content must be gone"
    );
    assert_eq!(
        db.search_history(scope_of("proj"), "wal".into())
            .await
            .unwrap()["returned"],
        0
    );
    // Terminal: a source-deleted entry cannot be forgotten, re-deleted, or re-indexed back.
    assert_eq!(
        db.forget_history_document(artifact.clone(), false)
            .await
            .unwrap(),
        "source_deleted"
    );
    assert_eq!(
        db.delete_history_source(artifact.clone()).await.unwrap(),
        "already_deleted"
    );
    db.index_history(100).await.unwrap();
    assert_eq!(
        db.history_document_audit(artifact).await.unwrap().unwrap()["content_present"],
        false,
        "re-indexing must not resurrect deleted content"
    );

    // A turn with no receipt or provenance edge has its row removed outright.
    assert_eq!(
        db.delete_history_source(turn.clone()).await.unwrap(),
        "source_deleted"
    );
    let remaining: i64 = db
        .read(|c| {
            Ok(
                c.query_row("SELECT count(*) FROM messages WHERE id=?1", [TURN], |r| {
                    r.get(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(remaining, 0);
    assert!(
        db.history_document_audit(turn).await.unwrap().is_some(),
        "the audit of a deleted turn must remain readable"
    );
}

/// Clause 3 and 4. An export must be reviewed before it can leave, round-trips with stable
/// ids and revisions, and importing the same packet twice changes nothing.
#[tokio::test]
async fn export_round_trips_idempotently_and_refuses_unreviewed_or_unsanitized_data() {
    let db = DbStore::init(":memory:").unwrap();
    seed(&db).await;
    db.index_history(100).await.unwrap();
    let document: String = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT id FROM history_documents WHERE kind='artifact'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();

    let bundle = db
        .create_export_bundle(
            "continuation_packet".into(),
            "proj".into(),
            "team".into(),
            vec![MEMORY.to_string()],
            vec![document.clone()],
            Some("handing off the durability work".into()),
        )
        .await
        .unwrap();
    assert_eq!(bundle["state"], "draft");
    assert_eq!(bundle["item_count"], 2);
    assert_eq!(bundle["unsanitized_items"], 0);
    let bundle_id = bundle["bundle_id"].as_str().unwrap().to_string();

    // An unreviewed bundle cannot leave.
    let refused = db
        .release_export_bundle(bundle_id.clone(), None)
        .await
        .unwrap();
    assert_eq!(refused["outcome"], "not_reviewed", "{refused}");

    // The review step shows exactly what would leave, and the digest to approve it against.
    let preview = db
        .review_export_bundle(bundle_id.clone(), false, None)
        .await
        .unwrap();
    assert_eq!(preview["outcome"], "preview");
    let digest = preview["content_sha256"].as_str().unwrap().to_string();
    // Approving without the digest, or with a stale one, is refused.
    assert_eq!(
        db.review_export_bundle(bundle_id.clone(), true, None)
            .await
            .unwrap()["outcome"],
        "missing_digest"
    );
    assert_eq!(
        db.review_export_bundle(bundle_id.clone(), true, Some("0".repeat(64)))
            .await
            .unwrap()["outcome"],
        "stale_review"
    );
    assert_eq!(
        db.review_export_bundle(bundle_id.clone(), true, Some(digest.clone()))
            .await
            .unwrap()["outcome"],
        "reviewed"
    );

    let released = db
        .release_export_bundle(bundle_id.clone(), Some("anchor-token".into()))
        .await
        .unwrap();
    assert_eq!(released["outcome"], "released");
    let packet_value = released["packet"].clone();
    let packet: ContinuationPacket = serde_json::from_value(packet_value.clone()).unwrap();
    packet.verify().unwrap();
    assert_eq!(packet.format_version, FORMAT_VERSION);
    assert_eq!(packet.bundle_id, bundle_id);
    assert_eq!(packet.audience, "team");
    assert_eq!(packet.anchor.as_deref(), Some("anchor-token"));
    assert_eq!(packet.items.len(), 2);
    // Stable ids and revisions, not freshly minted ones.
    assert!(packet
        .items
        .iter()
        .any(|item| item.stable_id == MEMORY && item.revision == 1));
    assert!(packet
        .items
        .iter()
        .any(|item| item.stable_id == document && item.revision == 1));
    assert!(!packet_value.to_string().contains("sk-abcdefghijklmnop"));

    // Releasing again is the same packet, not a second differently-identified export.
    let again = db
        .release_export_bundle(bundle_id.clone(), Some("anchor-token".into()))
        .await
        .unwrap();
    assert_eq!(
        again["packet"]["content_sha256"],
        packet_value["content_sha256"]
    );

    // --- Import into a second database: the round trip.
    let target = DbStore::init(":memory:").unwrap();
    let first = target
        .import_continuation_packet("remote".into(), packet_value.clone())
        .await
        .unwrap();
    assert_eq!(first["accepted"], 1, "{first}");
    // A ported memory is not silently activated: approval is an owner act.
    assert_eq!(first["skipped"], 1, "{first}");
    let outcomes: Vec<&str> = first["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["outcome"].as_str().unwrap())
        .collect();
    assert!(outcomes.contains(&"created"));
    assert!(outcomes.contains(&"skipped_unreviewed"));
    let imported = target
        .history_document_audit(document.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(imported["citation"]["id"], document, "stable id preserved");
    assert_eq!(imported["citation"]["revision"], 1, "revision preserved");

    // Idempotence: the identical packet again creates nothing and overwrites nothing.
    let second = target
        .import_continuation_packet("remote".into(), packet_value.clone())
        .await
        .unwrap();
    assert_eq!(second["accepted"], 0, "{second}");
    assert_eq!(second["unchanged"], 1, "{second}");
    let count: i64 = target
        .read(|c| Ok(c.query_row("SELECT count(*) FROM history_documents", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(count, 1, "re-import must not duplicate");

    // A lower revision never overwrites newer local content.
    let mut stale: ContinuationPacket = serde_json::from_value(packet_value.clone()).unwrap();
    target
            .run({
                let id = document.clone();
                move |c| {
                    c.execute(
                        "UPDATE history_documents SET revision=5,body='newer local text',content_sha256=?1 WHERE id=?2",
                        params![review::checksum("newer local text"), id],
                    )?;
                    Ok(json!({}))
                }
            })
            .await
            .unwrap();
    stale.items.retain(|item| item.kind == "history");
    let stale = stale.seal().unwrap();
    let outcome = target
        .import_continuation_packet("remote".into(), serde_json::to_value(&stale).unwrap())
        .await
        .unwrap();
    assert_eq!(outcome["decisions"][0]["outcome"], "skipped_stale");
    let body: String = target
        .read({
            let id = document.clone();
            move |c| {
                Ok(c.query_row(
                    "SELECT body FROM history_documents WHERE id=?1",
                    [&id],
                    |r| r.get(0),
                )?)
            }
        })
        .await
        .unwrap();
    assert_eq!(
        body, "newer local text",
        "an import must not overwrite newer local content"
    );
}

/// The reviewed digest is load-bearing, not decorative: contents that change after review
/// must fail at release rather than leave on the strength of an older look.
#[tokio::test]
async fn contents_changed_after_review_are_refused_at_release() {
    let db = DbStore::init(":memory:").unwrap();
    seed(&db).await;
    let bundle = db
        .create_export_bundle(
            "memory_selection".into(),
            "proj".into(),
            "self".into(),
            vec![MEMORY.to_string()],
            vec![],
            None,
        )
        .await
        .unwrap();
    let bundle_id = bundle["bundle_id"].as_str().unwrap().to_string();
    let digest = db
        .review_export_bundle(bundle_id.clone(), false, None)
        .await
        .unwrap()["content_sha256"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        db.review_export_bundle(bundle_id.clone(), true, Some(digest))
            .await
            .unwrap()["outcome"],
        "reviewed"
    );
    // Swap the reviewed payload for a different one, keeping every review flag intact. This
    // is the substitution the digest exists to catch.
    db.run({
        let id = bundle_id.clone();
        move |c| {
            let swapped = "something the operator never saw";
            c.execute(
                "UPDATE export_items SET payload_json=?1,content_sha256=?2 WHERE bundle_id=?3",
                params![
                    json!({"key":"language","body":swapped,"citation":{}}).to_string(),
                    review::checksum(swapped),
                    id
                ],
            )?;
            Ok(json!({}))
        }
    })
    .await
    .unwrap();
    let refused = db
        .release_export_bundle(bundle_id.clone(), None)
        .await
        .unwrap();
    assert_eq!(refused["outcome"], "stale_review", "{refused}");
    // And re-reviewing needs the *new* digest: the old approval cannot be replayed.
    let fresh = db
        .review_export_bundle(bundle_id.clone(), false, None)
        .await
        .unwrap()["content_sha256"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        db.review_export_bundle(bundle_id, true, Some(fresh))
            .await
            .unwrap()["outcome"],
        "reviewed"
    );
}

/// Clause 4, negative half. An export refuses to carry content the shared sanitizer rejects,
/// or a document the owner has forgotten or source-deleted.
#[tokio::test]
async fn an_export_refuses_unsanitized_forgotten_or_deleted_content() {
    let db = DbStore::init(":memory:").unwrap();
    seed(&db).await;
    db.index_history(100).await.unwrap();
    let document: String = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT id FROM history_documents WHERE kind='artifact'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();

    // A memory whose value the sanitizer rejects is recorded as refused, never shipped.
    db.run(|c| {
        c.execute(
            "UPDATE memories SET value='the api_key is sk-abcdefghijklmnop' WHERE id=?1",
            [MEMORY],
        )?;
        Ok(json!({}))
    })
    .await
    .unwrap();
    let bundle = db
        .create_export_bundle(
            "memory_selection".into(),
            "proj".into(),
            "public".into(),
            vec![MEMORY.to_string()],
            vec![],
            None,
        )
        .await
        .unwrap();
    assert_eq!(bundle["unsanitized_items"], 1, "{bundle}");
    assert!(!bundle.to_string().contains("sk-abcdefghijklmnop"));
    let bundle_id = bundle["bundle_id"].as_str().unwrap().to_string();
    assert_eq!(
        db.review_export_bundle(bundle_id.clone(), true, Some("0".repeat(64)))
            .await
            .unwrap()["outcome"],
        "unsanitized_items"
    );
    assert_eq!(
        db.release_export_bundle(bundle_id, None).await.unwrap()["outcome"],
        "not_reviewed"
    );

    // A forgotten document is withheld from an export: exporting is exactly the act the
    // owner said should stop happening.
    db.forget_history_document(document.clone(), false)
        .await
        .unwrap();
    let bundle = db
        .create_export_bundle(
            "continuation_packet".into(),
            "proj".into(),
            "self".into(),
            vec![],
            vec![document.clone()],
            None,
        )
        .await
        .unwrap();
    assert_eq!(bundle["unsanitized_items"], 1, "{bundle}");
    assert!(!bundle.to_string().contains("WAL"));

    // Selecting something outside the scope is an error, not a silent empty bundle.
    assert!(db
        .create_export_bundle(
            "continuation_packet".into(),
            "other".into(),
            "self".into(),
            vec![],
            vec![document],
            None,
        )
        .await
        .is_err());
    // Unknown kind, unknown audience and an empty selection are all refused.
    assert!(db
        .create_export_bundle(
            "backup".into(),
            "proj".into(),
            "self".into(),
            vec![MEMORY.into()],
            vec![],
            None
        )
        .await
        .is_err());
    assert!(db
        .create_export_bundle(
            "memory_selection".into(),
            "proj".into(),
            "everyone".into(),
            vec![MEMORY.into()],
            vec![],
            None
        )
        .await
        .is_err());
    assert!(db
        .create_export_bundle(
            "memory_selection".into(),
            "proj".into(),
            "self".into(),
            vec![],
            vec![],
            None
        )
        .await
        .is_err());
}

/// An imported packet is re-checked against the *local* sanitizer, because "it was reviewed
/// elsewhere" is a claim about another operator's judgement, not a local guarantee.
#[tokio::test]
async fn an_import_refuses_a_packet_that_fails_the_local_gate() {
    let db = DbStore::init(":memory:").unwrap();
    let citation = Citation {
        id: "66666666-6666-4666-8666-666666666666".into(),
        kind: "artifact".into(),
        source_id: "src".into(),
        revision: 1,
        scope: "proj".into(),
        session_id: None,
        timestamp: "2026-09-15T00:00:00+00:00".into(),
        content_sha256: review::checksum("bearer abc123def456"),
        sanitizer: review::SANITIZER.into(),
    };
    let packet = ContinuationPacket {
            format_version: FORMAT_VERSION,
            bundle_id: "77777777-7777-4777-8777-777777777777".into(),
            kind: PacketKind::ContinuationPacket,
            scope: "proj".into(),
            audience: "team".into(),
            sanitizer: review::SANITIZER.into(),
            created_at: "2026-09-15T00:00:00+00:00".into(),
            reviewed_at: "2026-09-15T00:01:00+00:00".into(),
            anchor: None,
            items: vec![PacketItem {
                kind: "history".into(),
                stable_id: citation.id.clone(),
                revision: 1,
                payload: json!({"title":"leak","body":"bearer abc123def456","citation":citation.to_json()}),
                citation,
                content_sha256: review::checksum("bearer abc123def456"),
            }],
            content_sha256: String::new(),
        }
        .seal()
        .unwrap();
    let error = db
        .import_continuation_packet("remote".into(), serde_json::to_value(&packet).unwrap())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("sanitizer refuses"), "{error}");
    let count: i64 = db
        .read(|c| Ok(c.query_row("SELECT count(*) FROM history_documents", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(count, 0, "a refused packet must write nothing");

    // A tampered checksum is refused before any content is looked at.
    let mut tampered = packet;
    tampered.content_sha256 = "0".repeat(64);
    assert!(db
        .import_continuation_packet("remote".into(), serde_json::to_value(&tampered).unwrap())
        .await
        .is_err());
}
