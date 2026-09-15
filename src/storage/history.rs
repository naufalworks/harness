//! P15-T04 seam: scoped search over sanitized history, the forget / source-delete boundary,
//! and the durable export/import ledger.
//!
//! Sanitization and citation building are not implemented here. They live in
//! `crate::export::review` and are shared with the export path, so a document search would
//! refuse can never be exported and a citation means the same thing on both sides.

use super::{now, uid, DbStore};
use crate::export::packet::{
    canonical_json, item_body, ContinuationPacket, PacketItem, PacketKind, FORMAT_VERSION,
};
use crate::export::review::{self, Citation};
use crate::safety;
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

/// The most hits one search returns, and the snippet length. Bounded so a search over a
/// production-sized history cannot become an unbounded read.
const SEARCH_LIMIT: usize = 50;
const SNIPPET_BYTES: usize = 240;
/// The most items one bundle may carry.
pub const MAX_BUNDLE_ITEMS: usize = 500;

/// A search scope. Every field is optional except the caller's project scope, which is always
/// applied: search is scoped by construction, never by the caller remembering to filter.
#[derive(Clone, Debug, Default)]
pub struct SearchScope {
    pub scope: String,
    pub session_id: Option<String>,
    /// `turn`, `artifact`, or `None` for both.
    pub kind: Option<String>,
}

/// Read one indexed document row into a citation plus its body.
fn document_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(Citation, String, String)> {
    let citation = Citation {
        id: row.get(0)?,
        kind: row.get(1)?,
        source_id: row.get(2)?,
        revision: row.get(3)?,
        scope: row.get(4)?,
        session_id: row.get(5)?,
        timestamp: row.get(6)?,
        content_sha256: row.get(7)?,
        sanitizer: row.get(8)?,
    };
    Ok((citation, row.get(9)?, row.get(10)?))
}

const DOCUMENT_COLUMNS: &str = "d.id,d.kind,d.source_id,d.revision,d.scope,d.session_id,d.source_created_at,d.content_sha256,d.sanitizer,d.title,d.body";

/// One document row as the privacy operations read it: kind, source id, revision, when it was
/// forgotten, when its source was deleted.
type DocumentState = (String, String, i64, Option<String>, Option<String>);

/// One bundle header as release reads it: kind, scope, audience, state, reviewed digest,
/// reviewed_at, created_at.
type BundleHeader = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
);

/// Cut a snippet on a character boundary and say when it was cut. A chopped body that pretends
/// to be complete is worse than an obviously truncated one.
fn snippet(body: &str) -> (String, bool) {
    if body.len() <= SNIPPET_BYTES {
        return (body.to_string(), false);
    }
    let mut end = SNIPPET_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    (body[..end].to_string(), true)
}

impl DbStore {
    /// Project sanitized conversation turns and artifacts into the search index.
    ///
    /// Explicitly maintained rather than trigger-driven, because only Rust can decide whether
    /// text is sanitized: a trigger would have to index the raw column. The FTS index itself is
    /// then kept in sync by triggers on this projection, exactly as `memory_fts` is.
    ///
    /// Re-indexing changed content advances `revision` and rewrites the body; unchanged content
    /// (same checksum) is left completely alone, so an idle sweep writes nothing. A document
    /// whose source was deleted is never resurrected.
    pub async fn index_history(&self, limit: i64) -> Result<Value> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            let mut indexed = 0i64;
            let mut advanced = 0i64;
            let mut unchanged = 0i64;
            let mut refused: Vec<Value> = Vec::new();

            // (kind, id, scope, session, role, title, raw body, created_at)
            type Pending = (
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                String,
                String,
                String,
            );
            let mut pending: Vec<Pending> = Vec::new();
            {
                let mut stmt = tx.prepare("SELECT m.id,s.scope,m.session_id,m.role,m.content,m.created_at FROM messages m JOIN sessions s ON s.id=m.session_id WHERE m.status='complete' ORDER BY m.seq LIMIT ?1")?;
                for row in stmt.query_map([limit], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                })? {
                    let (id, scope, session, role, content, created) = row?;
                    let title = format!("{role} turn");
                    pending.push((
                        "turn".into(),
                        id,
                        scope,
                        Some(session),
                        Some(role),
                        title,
                        content,
                        created,
                    ));
                }
                let mut stmt = tx.prepare("SELECT id,scope,name,content,created_at FROM sources ORDER BY created_at,id LIMIT ?1")?;
                for row in stmt.query_map([limit], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })? {
                    let (id, scope, name, content, created) = row?;
                    pending.push((
                        "artifact".into(),
                        id,
                        scope,
                        None,
                        None,
                        name,
                        content,
                        created,
                    ));
                }
            }

            for (kind, source_id, scope, session, role, title, raw, created_at) in pending {
                let clean = match review::sanitize(&raw) {
                    Ok(clean) => clean,
                    Err(rejection) => {
                        refused.push(json!({
                            "kind": kind,
                            "source_id": source_id,
                            "reason": rejection.code(),
                            "note": rejection.message(),
                        }));
                        continue;
                    }
                };
                let hash = review::checksum(&clean);
                let existing: Option<(String, i64, String, Option<String>)> = tx
                    .query_row(
                        "SELECT id,revision,content_sha256,source_deleted_at FROM history_documents WHERE kind=?1 AND source_id=?2",
                        params![kind, source_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()?;
                match existing {
                    // A source-deleted document is terminal: re-indexing it would undo the
                    // deletion the audit trail says happened.
                    Some((_, _, _, Some(_))) => unchanged += 1,
                    Some((id, revision, stored_hash, None)) if stored_hash == hash => {
                        let _ = (id, revision);
                        unchanged += 1;
                    }
                    Some((id, revision, _, None)) => {
                        let next = revision + 1;
                        tx.execute("UPDATE history_documents SET revision=?1,title=?2,body=?3,sanitizer=?4,content_sha256=?5,indexed_at=?6 WHERE id=?7",
                            params![next, title, clean, review::SANITIZER, hash, stamp, id])?;
                        tx.execute("INSERT INTO history_privacy_events(id,document_id,kind,source_id,action,revision,detail,created_at) VALUES(?1,?2,?3,?4,'index',?5,'content changed',?6)",
                            params![uid(), id, kind, source_id, next, stamp])?;
                        advanced += 1;
                    }
                    None => {
                        let id = uid();
                        tx.execute("INSERT INTO history_documents(id,kind,source_id,scope,session_id,revision,role,title,body,sanitizer,content_sha256,source_created_at,indexed_at) VALUES(?1,?2,?3,?4,?5,1,?6,?7,?8,?9,?10,?11,?12)",
                            params![id, kind, source_id, scope, session, role, title, clean, review::SANITIZER, hash, created_at, stamp])?;
                        tx.execute("INSERT INTO history_privacy_events(id,document_id,kind,source_id,action,revision,detail,created_at) VALUES(?1,?2,?3,?4,'index',1,'first index',?5)",
                            params![uid(), id, kind, source_id, stamp])?;
                        indexed += 1;
                    }
                }
            }
            tx.commit()?;
            Ok(json!({
                "format_version": 1,
                "indexed": indexed,
                "revision_advanced": advanced,
                "unchanged": unchanged,
                "refused": refused,
                "sanitizer": review::SANITIZER,
                "note": "Only content the shared sanitizer accepted is indexed. Refused rows are reported, never indexed."
            }))
        })
        .await
    }

    /// Scoped FTS search over sanitized history. Every hit carries a citation naming the source
    /// row, its revision, its kind and its timestamp.
    ///
    /// Three gates keep unsanitized content out of a result, deliberately overlapping:
    /// the index only ever received sanitized text; forgotten and source-deleted rows are
    /// excluded in SQL; and each body is re-checked with the shared sanitizer at read time, so a
    /// row written by an older sanitizer or edited out of band is dropped rather than served.
    pub async fn search_history(&self, scope: SearchScope, query: String) -> Result<Value> {
        let fts = safety::fts_query(&query);
        self.read(move |c| {
            if fts.is_empty() {
                return Ok(json!({
                    "format_version": 1,
                    "scope": scope.scope,
                    "session_id": scope.session_id,
                    "kind": scope.kind,
                    "query": query,
                    "hits": [],
                    "returned": 0,
                    "suppressed": 0,
                    "note": "The query had no searchable terms, so nothing was matched."
                }));
            }
            let sql = format!("SELECT {DOCUMENT_COLUMNS},bm25(history_fts) FROM history_fts JOIN history_documents d ON d.rowid=history_fts.rowid WHERE history_fts MATCH ?1 AND d.scope=?2 AND (?3 IS NULL OR d.session_id=?3) AND (?4 IS NULL OR d.kind=?4) AND d.forgotten_at IS NULL AND d.source_deleted_at IS NULL ORDER BY bm25(history_fts),d.source_created_at DESC LIMIT ?5");
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt
                .query_map(
                    params![
                        fts,
                        scope.scope,
                        scope.session_id,
                        scope.kind,
                        SEARCH_LIMIT as i64
                    ],
                    |r| Ok((document_row(r)?, r.get::<_, f64>(11)?)),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut hits = Vec::new();
            let mut suppressed = 0i64;
            for ((citation, title, body), rank) in rows {
                // Read-time gate. A stored body must still be exactly what the shared sanitizer
                // would emit, and must still match the checksum its citation advertises.
                if !review::is_sanitized(&body) || review::checksum(&body) != citation.content_sha256
                {
                    suppressed += 1;
                    continue;
                }
                let (text, truncated) = snippet(&body);
                hits.push(json!({
                    "title": title,
                    "snippet": text,
                    "snippet_truncated": truncated,
                    "rank": rank,
                    "citation": citation.to_json(),
                }));
            }
            Ok(json!({
                "format_version": 1,
                "scope": scope.scope,
                "session_id": scope.session_id,
                "kind": scope.kind,
                "query": query,
                "hits": hits.len(),
                "results": hits,
                "returned": hits.len(),
                "suppressed": suppressed,
                "limit": SEARCH_LIMIT,
                "sanitizer": review::SANITIZER,
                "note": "Only sanitized, not-forgotten, not-source-deleted documents are searched. Suppressed rows failed the read-time sanitizer or checksum gate."
            }))
        })
        .await
    }

    /// Forget a document: stop it being recalled or returned, while keeping the row, its
    /// citation and its revision trail. This is *not* a deletion, and `restore` exists precisely
    /// because the evidence was never destroyed.
    ///
    /// Returns `not_found`, `forgotten`, `already_forgotten`, `restored`, `not_forgotten` or
    /// `source_deleted` — the last because a source-deleted document has nothing left to forget.
    pub async fn forget_history_document(&self, id: String, restore: bool) -> Result<String> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row: Option<DocumentState> = tx
                .query_row("SELECT kind,source_id,revision,forgotten_at,source_deleted_at FROM history_documents WHERE id=?1",
                    [&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
                .optional()?;
            let Some((kind, source_id, revision, forgotten_at, source_deleted_at)) = row else {
                return Ok("not_found".to_string());
            };
            if source_deleted_at.is_some() {
                return Ok("source_deleted".to_string());
            }
            let stamp = now();
            let (outcome, action, detail) = match (restore, forgotten_at.is_some()) {
                (false, true) => return Ok("already_forgotten".to_string()),
                (true, false) => return Ok("not_forgotten".to_string()),
                (false, false) => (
                    "forgotten",
                    "forget",
                    "suppressed from recall and search; content and revision trail retained",
                ),
                (true, true) => (
                    "restored",
                    "restore",
                    "suppression lifted; content was never destroyed",
                ),
            };
            let value: Option<String> = if restore { None } else { Some(stamp.clone()) };
            tx.execute(
                "UPDATE history_documents SET forgotten_at=?1 WHERE id=?2",
                params![value, id],
            )?;
            tx.execute("INSERT INTO history_privacy_events(id,document_id,kind,source_id,action,revision,detail,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![uid(), id, kind, source_id, action, revision, detail, stamp])?;
            tx.commit()?;
            Ok(outcome.to_string())
        })
        .await
    }

    /// Delete the underlying source content.
    ///
    /// This is the operation `forget` is not. The indexed body is emptied and the source row
    /// itself is removed, so the content is gone from this database; the `history_documents` row
    /// and its append-only events stay, so the fact that the entry existed and was deleted
    /// remains provable. A turn that is still referenced by a receipt or provenance edge cannot
    /// have its source row removed — the body is still emptied and the deletion still audited,
    /// and the outcome says `content_removed_source_retained` rather than pretending.
    pub async fn delete_history_source(&self, id: String) -> Result<String> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row: Option<(String, String, i64, Option<String>)> = tx
                .query_row("SELECT kind,source_id,revision,source_deleted_at FROM history_documents WHERE id=?1",
                    [&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .optional()?;
            let Some((kind, source_id, revision, already)) = row else {
                return Ok("not_found".to_string());
            };
            if already.is_some() {
                return Ok("already_deleted".to_string());
            }
            let stamp = now();
            // The projection is emptied first, so no ordering of the following statements can
            // leave a searchable body behind.
            tx.execute(
                "UPDATE history_documents SET body='',source_deleted_at=?1 WHERE id=?2",
                params![stamp, id],
            )?;
            let removed = match kind.as_str() {
                "turn" => {
                    // Clear the recorded text unconditionally, then remove the row only when
                    // nothing durable still references it.
                    tx.execute(
                        "UPDATE messages SET content='' WHERE id=?1",
                        [&source_id],
                    )?;
                    let referenced: i64 = tx.query_row(
                        "SELECT (SELECT count(*) FROM chat_receipts WHERE request_id=?1 OR answer_id=?1)+(SELECT count(*) FROM provenance_edges WHERE (source_kind='evidence' AND source_id=?1) OR (target_kind='evidence' AND target_id=?1))",
                        [&source_id],
                        |r| r.get(0),
                    )?;
                    if referenced == 0 {
                        tx.execute("DELETE FROM messages WHERE id=?1", [&source_id])? > 0
                    } else {
                        false
                    }
                }
                _ => {
                    let referenced: i64 = tx.query_row(
                        "SELECT (SELECT count(*) FROM candidates WHERE source_id=?1)+(SELECT count(*) FROM jobs WHERE source_id=?1)",
                        [&source_id],
                        |r| r.get(0),
                    )?;
                    if referenced == 0 {
                        tx.execute("DELETE FROM sources WHERE id=?1", [&source_id])? > 0
                    } else {
                        tx.execute("UPDATE sources SET content='' WHERE id=?1", [&source_id])?;
                        false
                    }
                }
            };
            let detail = if removed {
                "source row removed; audit retained"
            } else {
                "source content emptied, row retained because durable evidence still references it; audit retained"
            };
            tx.execute("INSERT INTO history_privacy_events(id,document_id,kind,source_id,action,revision,detail,created_at) VALUES(?1,?2,?3,?4,'delete_source',?5,?6,?7)",
                params![uid(), id, kind, source_id, revision, detail, stamp])?;
            tx.commit()?;
            Ok(if removed {
                "source_deleted".to_string()
            } else {
                "content_removed_source_retained".to_string()
            })
        })
        .await
    }

    /// The audit trail of one document: what it cites, its current suppression/deletion state,
    /// and every recorded act in order. After a forget this still shows the entry existed; after
    /// a source delete it still shows the deletion, with the body gone.
    pub async fn history_document_audit(&self, id: String) -> Result<Option<Value>> {
        self.read(move |c| {
            let sql = format!("SELECT {DOCUMENT_COLUMNS},d.forgotten_at,d.source_deleted_at,d.indexed_at FROM history_documents d WHERE d.id=?1");
            let row = c
                .query_row(&sql, [&id], |r| {
                    Ok((
                        document_row(r)?,
                        r.get::<_, Option<String>>(11)?,
                        r.get::<_, Option<String>>(12)?,
                        r.get::<_, String>(13)?,
                    ))
                })
                .optional()?;
            let Some(((citation, title, body), forgotten_at, source_deleted_at, indexed_at)) = row
            else {
                return Ok(None);
            };
            let mut stmt = c.prepare("SELECT action,revision,detail,created_at FROM history_privacy_events WHERE document_id=?1 ORDER BY seq LIMIT 500")?;
            let events = stmt
                .query_map([&id], |r| {
                    Ok(json!({"action":r.get::<_,String>(0)?,"revision":r.get::<_,i64>(1)?,
                        "detail":r.get::<_,Option<String>>(2)?,"created_at":r.get::<_,String>(3)?}))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Some(json!({
                "format_version": 1,
                "citation": citation.to_json(),
                "title": title,
                "indexed_at": indexed_at,
                "forgotten_at": forgotten_at,
                "source_deleted_at": source_deleted_at,
                "searchable": forgotten_at.is_none() && source_deleted_at.is_none(),
                "content_present": !body.is_empty(),
                "events": events,
                "note": "A forgotten entry keeps its content and revision trail and stops being returned. A source-deleted entry keeps only this record of having existed and been deleted."
            })))
        })
        .await
    }

    /// Assemble a draft export bundle from explicitly selected memories and history documents.
    ///
    /// Nothing is reviewed by this call and nothing can leave yet. Items the shared sanitizer
    /// refuses are still recorded, with `sanitized=0`, so the reviewer sees what was considered;
    /// release then refuses while any of them is present.
    pub async fn create_export_bundle(
        &self,
        kind: String,
        scope: String,
        audience: String,
        memory_ids: Vec<String>,
        document_ids: Vec<String>,
        note: Option<String>,
    ) -> Result<Value> {
        let packet_kind =
            PacketKind::parse(&kind).ok_or_else(|| anyhow::anyhow!("unsupported bundle kind"))?;
        if !matches!(audience.as_str(), "self" | "team" | "public") {
            bail!("unsupported audience");
        }
        if memory_ids.len() + document_ids.len() > MAX_BUNDLE_ITEMS {
            bail!("a bundle carries at most {MAX_BUNDLE_ITEMS} items");
        }
        if memory_ids.is_empty() && document_ids.is_empty() {
            bail!("a bundle needs at least one selected item");
        }
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            let bundle_id = uid();
            let mut items: Vec<Value> = Vec::new();
            let mut refused = 0i64;
            // The header is written first: `export_items.bundle_id` is a foreign key, so an
            // item cannot reference a bundle that does not exist yet. `item_count` is corrected
            // below once the selection has actually been resolved.
            tx.execute("INSERT INTO export_bundles(id,kind,scope,audience,state,format_version,item_count,note,created_at,updated_at) VALUES(?1,?2,?3,?4,'draft',1,0,?5,?6,?6)",
                params![bundle_id, packet_kind.as_str(), scope, audience, note, stamp])?;

            for memory_id in &memory_ids {
                let row: Option<(String, String, i64, String, String, String)> = tx
                    .query_row("SELECT key,value,revision,category,branch,updated_at FROM memories WHERE id=?1 AND scope=?2",
                        params![memory_id, scope],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
                    .optional()?;
                let Some((key, value, revision, category, branch, updated_at)) = row else {
                    bail!("memory {memory_id} is not in scope {scope}");
                };
                let (clean, sanitized) = match review::sanitize(&value) {
                    Ok(clean) => (clean, true),
                    Err(rejection) => (rejection.code().to_string(), false),
                };
                let hash = review::checksum(&clean);
                let citation = Citation {
                    id: memory_id.clone(),
                    kind: "memory".into(),
                    source_id: memory_id.clone(),
                    revision,
                    scope: scope.clone(),
                    session_id: None,
                    timestamp: updated_at,
                    content_sha256: hash.clone(),
                    sanitizer: review::SANITIZER.into(),
                };
                let payload = json!({
                    "key": key,
                    "body": clean,
                    "category": category,
                    "branch": branch,
                    "citation": citation.to_json(),
                });
                if !sanitized {
                    refused += 1;
                }
                tx.execute("INSERT INTO export_items(bundle_id,kind,stable_id,revision,payload_json,content_sha256,sanitized,reviewed,created_at) VALUES(?1,'memory',?2,?3,?4,?5,?6,0,?7)",
                    params![bundle_id, memory_id, revision, payload.to_string(), hash, i64::from(sanitized), stamp])?;
                items.push(json!({"kind":"memory","stable_id":memory_id,"revision":revision,"sanitized":sanitized,"payload":payload}));
            }

            for document_id in &document_ids {
                let sql = format!("SELECT {DOCUMENT_COLUMNS},d.forgotten_at,d.source_deleted_at FROM history_documents d WHERE d.id=?1 AND d.scope=?2");
                let row = tx
                    .query_row(&sql, params![document_id, scope], |r| {
                        Ok((
                            document_row(r)?,
                            r.get::<_, Option<String>>(11)?,
                            r.get::<_, Option<String>>(12)?,
                        ))
                    })
                    .optional()?;
                let Some(((citation, title, body), forgotten_at, deleted_at)) = row else {
                    bail!("history document {document_id} is not in scope {scope}");
                };
                // Forgotten and source-deleted documents are refused at selection: an export is
                // exactly the act the owner said should stop happening.
                let (clean, sanitized) = if forgotten_at.is_some() || deleted_at.is_some() {
                    ("withheld_forgotten_or_deleted".to_string(), false)
                } else {
                    match review::sanitize(&body) {
                        Ok(clean) => (clean, true),
                        Err(rejection) => (rejection.code().to_string(), false),
                    }
                };
                let hash = review::checksum(&clean);
                let payload = json!({
                    "title": title,
                    "body": clean,
                    "citation": citation.to_json(),
                });
                if !sanitized {
                    refused += 1;
                }
                tx.execute("INSERT INTO export_items(bundle_id,kind,stable_id,revision,payload_json,content_sha256,sanitized,reviewed,created_at) VALUES(?1,'history',?2,?3,?4,?5,?6,0,?7)",
                    params![bundle_id, document_id, citation.revision, payload.to_string(), hash, i64::from(sanitized), stamp])?;
                items.push(json!({"kind":"history","stable_id":document_id,"revision":citation.revision,"sanitized":sanitized,"payload":payload}));
            }

            let count = i64::try_from(items.len()).unwrap_or(i64::MAX);
            tx.execute(
                "UPDATE export_bundles SET item_count=?1 WHERE id=?2",
                params![count, bundle_id],
            )?;
            tx.commit()?;
            Ok(json!({
                "format_version": 1,
                "bundle_id": bundle_id,
                "kind": packet_kind.as_str(),
                "scope": scope,
                "audience": audience,
                "state": "draft",
                "item_count": count,
                "unsanitized_items": refused,
                "items": items,
                "note": "Nothing has left yet. Review this bundle, then release it; release refuses while any item is unreviewed or unsanitized."
            }))
        })
        .await
    }

    /// The audience-review step. The operator sees the bundle and approves it against a checksum
    /// over the exact contents they were shown; that checksum is stored and re-verified at
    /// release, so contents that change after review cannot leave.
    ///
    /// `expected_sha256` is the digest the reviewer was shown (from `review_export_bundle` with
    /// `approve=false`). Passing a stale one is a conflict, not a silent re-approval.
    pub async fn review_export_bundle(
        &self,
        bundle_id: String,
        approve: bool,
        expected_sha256: Option<String>,
    ) -> Result<Value> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let header: Option<(String, String, String, String, i64)> = tx
                .query_row("SELECT kind,scope,audience,state,item_count FROM export_bundles WHERE id=?1",
                    [&bundle_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
                .optional()?;
            let Some((kind, scope, audience, state, item_count)) = header else {
                return Ok(json!({"outcome":"not_found"}));
            };
            if state == "released" {
                return Ok(json!({"outcome":"already_released"}));
            }
            let (items, digest, unsanitized) = bundle_items(&tx, &bundle_id)?;
            if !approve {
                // A pure preview: what would leave, and the digest to approve it against.
                return Ok(json!({
                    "outcome": "preview",
                    "bundle_id": bundle_id,
                    "kind": kind,
                    "scope": scope,
                    "audience": audience,
                    "state": state,
                    "item_count": item_count,
                    "unsanitized_items": unsanitized,
                    "content_sha256": digest,
                    "items": items,
                    "note": "This is what would leave. Approve with this exact content_sha256 to record the review."
                }));
            }
            if unsanitized > 0 {
                return Ok(json!({"outcome":"unsanitized_items","unsanitized_items":unsanitized,
                    "note":"Remove or fix the refused items before reviewing; an export cannot carry content the sanitizer rejected."}));
            }
            if let Some(expected) = expected_sha256.as_deref() {
                if expected != digest {
                    return Ok(json!({"outcome":"stale_review","content_sha256":digest,
                        "note":"The bundle changed after it was shown. Re-read it and approve the current digest."}));
                }
            } else {
                return Ok(json!({"outcome":"missing_digest","content_sha256":digest,
                    "note":"Approving requires the digest of the contents that were reviewed."}));
            }
            let stamp = now();
            tx.execute("UPDATE export_items SET reviewed=1 WHERE bundle_id=?1", [&bundle_id])?;
            tx.execute("UPDATE export_bundles SET state='reviewed',content_sha256=?1,reviewed_at=?2,updated_at=?2 WHERE id=?3",
                params![digest, stamp, bundle_id])?;
            tx.commit()?;
            Ok(json!({
                "outcome": "reviewed",
                "bundle_id": bundle_id,
                "content_sha256": digest,
                "reviewed_at": stamp,
                "note": "The reviewed contents are pinned by this digest. Release will refuse if they change."
            }))
        })
        .await
    }

    /// Serialize a reviewed bundle into a sealed continuation packet.
    ///
    /// Refuses a draft, refuses an unreviewed or unsanitized item, and refuses contents whose
    /// digest no longer matches the reviewed one. Releasing twice returns the same packet: the
    /// bundle is already `released` and its contents are immutable, so re-release is idempotent
    /// rather than a second, differently-identified export.
    pub async fn release_export_bundle(
        &self,
        bundle_id: String,
        anchor: Option<String>,
    ) -> Result<Value> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let header: Option<BundleHeader> = tx
                .query_row("SELECT kind,scope,audience,state,content_sha256,reviewed_at,created_at FROM export_bundles WHERE id=?1",
                    [&bundle_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))
                .optional()?;
            let Some((kind, scope, audience, state, reviewed_digest, reviewed_at, created_at)) = header else {
                return Ok(json!({"outcome":"not_found"}));
            };
            if state == "draft" || state == "rejected" {
                return Ok(json!({"outcome":"not_reviewed",
                    "note":"An export must be reviewed before it can leave."}));
            }
            let (Some(reviewed_digest), Some(reviewed_at)) = (reviewed_digest, reviewed_at) else {
                return Ok(json!({"outcome":"not_reviewed"}));
            };
            let (items, digest, unsanitized) = bundle_items(&tx, &bundle_id)?;
            if unsanitized > 0 {
                return Ok(json!({"outcome":"unsanitized_items","unsanitized_items":unsanitized}));
            }
            let unreviewed: i64 = tx.query_row(
                "SELECT count(*) FROM export_items WHERE bundle_id=?1 AND reviewed=0",
                [&bundle_id],
                |r| r.get(0),
            )?;
            if unreviewed > 0 {
                return Ok(json!({"outcome":"unreviewed_items","unreviewed_items":unreviewed,
                    "note":"Every item must be reviewed before the bundle can leave."}));
            }
            if digest != reviewed_digest {
                return Ok(json!({"outcome":"stale_review","content_sha256":digest,
                    "note":"The contents changed after review, so the export was refused."}));
            }
            let packet_kind = PacketKind::parse(&kind)
                .ok_or_else(|| anyhow::anyhow!("stored bundle kind is unknown"))?;
            let mut packet_items = Vec::new();
            for item in &items {
                let payload = item["payload"].clone();
                let body = item_body(&payload);
                let citation: Citation = serde_json::from_value(payload["citation"].clone())?;
                packet_items.push(PacketItem {
                    kind: item["kind"].as_str().unwrap_or_default().to_string(),
                    stable_id: item["stable_id"].as_str().unwrap_or_default().to_string(),
                    revision: item["revision"].as_i64().unwrap_or_default(),
                    payload,
                    citation,
                    content_sha256: review::checksum(&body),
                });
            }
            let packet = ContinuationPacket {
                format_version: FORMAT_VERSION,
                bundle_id: bundle_id.clone(),
                kind: packet_kind,
                scope,
                audience,
                sanitizer: review::SANITIZER.into(),
                created_at,
                reviewed_at,
                anchor,
                items: packet_items,
                content_sha256: String::new(),
            }
            .seal()?;
            // Sealing without verifying would let a bug ship a packet this build itself refuses
            // to import, which is the one failure a round-trip promise cannot absorb.
            packet.verify()?;
            if state != "released" {
                let stamp = now();
                tx.execute("UPDATE export_bundles SET state='released',released_at=?1,updated_at=?1 WHERE id=?2",
                    params![stamp, bundle_id])?;
            }
            tx.commit()?;
            Ok(json!({
                "outcome": "released",
                "packet": serde_json::to_value(&packet)?,
                "note": "Reviewed sanitized evidence with stable ids and revisions. It reproduces no past answer and carries no exact original bytes."
            }))
        })
        .await
    }

    /// Import a continuation packet.
    ///
    /// Verified first (format, bundle checksum, per-item checksum, local sanitizer), then merged
    /// by stable id plus revision:
    ///
    /// * unknown id → `created`;
    /// * same id, same revision, same checksum → `unchanged` (nothing written);
    /// * same id, higher revision → `revision_advanced`;
    /// * same id, lower revision → `skipped_stale` (an import never overwrites newer local work).
    ///
    /// Re-importing the identical packet therefore produces a second receipt full of `unchanged`
    /// rows and changes no content: idempotent, and observably so.
    pub async fn import_continuation_packet(
        &self,
        origin: String,
        document: Value,
    ) -> Result<Value> {
        let packet: ContinuationPacket = serde_json::from_value(document)
            .map_err(|e| anyhow::anyhow!("packet is not a continuation packet: {e}"))?;
        packet.verify()?;
        if packet.items.len() > MAX_BUNDLE_ITEMS {
            bail!("a packet carries at most {MAX_BUNDLE_ITEMS} items");
        }
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            let receipt_id = uid();
            let mut accepted = 0i64;
            let mut unchanged = 0i64;
            let mut skipped = 0i64;
            let mut decisions = Vec::new();
            // Same ordering rule as the export side: `import_decisions.receipt_id` is a foreign
            // key, so the receipt row exists before any decision references it. Its counters are
            // corrected below, once the decisions have actually been made.
            tx.execute("INSERT INTO import_receipts(id,bundle_id,origin,scope,kind,content_sha256,accepted,unchanged,skipped,created_at) VALUES(?1,?2,?3,?4,?5,?6,0,0,0,?7)",
                params![receipt_id, packet.bundle_id, origin, packet.scope, packet.kind.as_str(),
                    packet.content_sha256, stamp])?;

            for item in &packet.items {
                let body = item_body(&item.payload);
                // Third gate, on this machine: the packet said it was reviewed elsewhere, which
                // is a claim about another operator's judgement, not a local guarantee.
                let outcome = if !review::is_sanitized(&body) {
                    "skipped_unsanitized"
                } else if item.kind != "history" {
                    // Memories are not written straight into `memories` by an import: approval
                    // is an owner act (docs/ARCHITECTURE.md "Memory approval"), so a ported
                    // memory is recorded as a decision for review rather than activated.
                    "skipped_unreviewed"
                } else {
                    let existing: Option<(i64, String, Option<String>)> = tx
                        .query_row("SELECT revision,content_sha256,source_deleted_at FROM history_documents WHERE id=?1",
                            [&item.stable_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                        .optional()?;
                    let hash = review::checksum(&body);
                    match existing {
                        None => {
                            let citation = &item.citation;
                            tx.execute("INSERT INTO history_documents(id,kind,source_id,scope,session_id,revision,role,title,body,sanitizer,content_sha256,source_created_at,indexed_at) VALUES(?1,'artifact',?2,?3,NULL,?4,NULL,?5,?6,?7,?8,?9,?10)",
                                params![item.stable_id, citation.source_id, packet.scope, item.revision,
                                    item.payload.get("title").and_then(Value::as_str).unwrap_or("imported document"),
                                    body, review::SANITIZER, hash, citation.timestamp, stamp])?;
                            tx.execute("INSERT INTO history_privacy_events(id,document_id,kind,source_id,action,revision,detail,created_at) VALUES(?1,?2,'artifact',?3,'index',?4,?5,?6)",
                                params![uid(), item.stable_id, citation.source_id, item.revision,
                                    format!("imported from {origin} bundle {}", packet.bundle_id), stamp])?;
                            "created"
                        }
                        // Terminal: a deletion recorded here outranks an incoming copy.
                        Some((_, _, Some(_))) => "skipped_stale",
                        Some((revision, stored_hash, None)) => {
                            if revision == item.revision && stored_hash == hash {
                                "unchanged"
                            } else if item.revision > revision {
                                tx.execute("UPDATE history_documents SET revision=?1,body=?2,content_sha256=?3,sanitizer=?4,indexed_at=?5 WHERE id=?6",
                                    params![item.revision, body, hash, review::SANITIZER, stamp, item.stable_id])?;
                                tx.execute("INSERT INTO history_privacy_events(id,document_id,kind,source_id,action,revision,detail,created_at) VALUES(?1,?2,'artifact',?3,'index',?4,?5,?6)",
                                    params![uid(), item.stable_id, item.citation.source_id, item.revision,
                                        format!("revision advanced by import from {origin}"), stamp])?;
                                "revision_advanced"
                            } else {
                                "skipped_stale"
                            }
                        }
                    }
                };
                match outcome {
                    "created" | "revision_advanced" => accepted += 1,
                    "unchanged" => unchanged += 1,
                    _ => skipped += 1,
                }
                let detail = match outcome {
                    "skipped_unreviewed" => Some("a ported memory needs local approval before it can be recalled"),
                    "skipped_stale" => Some("local content is newer, or was deleted here"),
                    "skipped_unsanitized" => Some("the local sanitizer refused this body"),
                    _ => None,
                };
                tx.execute("INSERT INTO import_decisions(receipt_id,kind,stable_id,revision,outcome,detail) VALUES(?1,?2,?3,?4,?5,?6)",
                    params![receipt_id, item.kind, item.stable_id, item.revision, outcome, detail])?;
                decisions.push(json!({"kind":item.kind,"stable_id":item.stable_id,
                    "revision":item.revision,"outcome":outcome,"detail":detail}));
            }

            tx.execute(
                "UPDATE import_receipts SET accepted=?1,unchanged=?2,skipped=?3 WHERE id=?4",
                params![accepted, unchanged, skipped, receipt_id],
            )?;
            tx.commit()?;
            Ok(json!({
                "format_version": 1,
                "receipt_id": receipt_id,
                "bundle_id": packet.bundle_id,
                "content_sha256": packet.content_sha256,
                "accepted": accepted,
                "unchanged": unchanged,
                "skipped": skipped,
                "decisions": decisions,
                "note": "Stable ids and revisions decided every outcome. Re-importing the same packet is unchanged, never a duplicate; ported memories still need local approval."
            }))
        })
        .await
    }
}

/// The bundle's items in a deterministic order, the digest over exactly them, and how many the
/// sanitizer refused. One helper so review, release and any preview hash the same bytes.
fn bundle_items(conn: &Connection, bundle_id: &str) -> Result<(Vec<Value>, String, i64)> {
    let mut stmt = conn.prepare("SELECT kind,stable_id,revision,payload_json,content_sha256,sanitized,reviewed FROM export_items WHERE bundle_id=?1 ORDER BY kind,stable_id")?;
    let rows = stmt
        .query_map([bundle_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut items = Vec::new();
    let mut unsanitized = 0i64;
    for (kind, stable_id, revision, payload, hash, sanitized, reviewed) in rows {
        if sanitized == 0 {
            unsanitized += 1;
        }
        items.push(json!({
            "kind": kind,
            "stable_id": stable_id,
            "revision": revision,
            "payload": serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null),
            "content_sha256": hash,
            "sanitized": sanitized == 1,
            "reviewed": reviewed == 1,
        }));
    }
    // The digest covers the items only, not the mutable review flags, so approving does not
    // invalidate the digest the reviewer just approved.
    let hashed = items
        .iter()
        .map(|item| {
            json!({"kind":item["kind"],"stable_id":item["stable_id"],
                "revision":item["revision"],"content_sha256":item["content_sha256"],
                "sanitized":item["sanitized"]})
        })
        .collect::<Vec<_>>();
    let digest = review::checksum(&canonical_json(&Value::Array(hashed)));
    Ok((items, digest, unsanitized))
}

#[cfg(test)]
mod tests {
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
}
