//! P12-T02 seam 3: the memory candidate repository. Compaction candidates, the
//! candidate feed, candidate edits, approve/reject resolution and hybrid recall
//! live here. The child module still uses the private `DbStore::run`/`read`
//! helpers, so no visibility was widened.
//!
//! P15-T02: ranking is factored into `rank_in_tx` so exactly one implementation answers three
//! questions — what a turn retrieved, why each candidate was kept or dropped, and what the same
//! ranking would return if a pending candidate were approved. A second implementation for the
//! preview would be a copy that drifts.

use super::{now, uid, DbStore, Recall, RecallCandidate};
use crate::safety;
use anyhow::{bail, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// The context payload ceiling recall has always enforced, and the number of ranked rows it
/// considers. Named so the receipt can report the same constants it was judged against.
const RECALL_BYTE_CEILING: usize = 6000;
const RECALL_RANK_LIMIT: usize = 20;

/// The stored receipt header, as one row:
/// scope, strategy, embedding model, prompt fingerprint, budget bytes, included bytes,
/// considered, included, created_at.
type ReceiptHeader = (String, String, String, String, i64, i64, i64, i64, String);

pub(crate) fn strategy_label(strategy: crate::embeddings::Strategy) -> &'static str {
    match strategy {
        crate::embeddings::Strategy::Hybrid => "hybrid",
        crate::embeddings::Strategy::LexicalOnly => "lexical_only",
    }
}

/// The branch checked out for this scope. No row means 'main', so a scope that has never used
/// branching keeps behaving exactly as it did before P15-T03.
pub(crate) fn active_branch(c: &rusqlite::Connection, scope: &str) -> Result<String> {
    Ok(c.query_row(
        "SELECT branch FROM memory_branches WHERE scope=?1 AND active=1",
        [scope],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .unwrap_or_else(|| "main".to_string()))
}

/// The decision group for one (scope, key, branch). Deriving it keeps `memories.conflict_group`
/// and `memory_decisions.group_id` the same concept instead of two that can disagree.
pub(crate) fn decision_group(scope: &str, key: &str, branch: &str) -> String {
    format!("{scope}\u{1f}{key}\u{1f}{branch}")
}

/// One memory row as the timeline reads it: key, value, branch, status, revision, pinned,
/// expires_at, conflict group, created_at, updated_at.
type GovernedMemory = (
    String,
    String,
    String,
    String,
    i64,
    i64,
    Option<i64>,
    Option<String>,
    String,
    String,
);

/// True when neither retrieval arm can admit anything, so recall must return nothing rather
/// than fall through to a full scan.
fn no_arm_can_match(
    fts: &str,
    query_vector: &[f32],
    strategy: crate::embeddings::Strategy,
) -> bool {
    let semantic_enabled = strategy == crate::embeddings::Strategy::Hybrid;
    fts.is_empty() && (!semantic_enabled || query_vector.iter().all(|value| *value == 0.0))
}

/// Rank the scope's active memories against one prompt inside an open transaction.
///
/// Returns the payload recall would hand to the context builder, plus one explanation row per
/// considered candidate. `persist` is false for previews: the embedding cache and the
/// `recall_count` counters are left untouched so looking does not change what is measured.
pub(crate) fn rank_in_tx(
    tx: &Transaction<'_>,
    scope: &str,
    prompt: &str,
    strategy: crate::embeddings::Strategy,
    persist: bool,
) -> Result<(Vec<Recall>, Vec<RecallCandidate>)> {
    let semantic_enabled = strategy == crate::embeddings::Strategy::Hybrid;
    let fts = safety::fts_query(prompt);
    let query_vector = crate::embeddings::embed(prompt);
    if no_arm_can_match(&fts, &query_vector, strategy) {
        return Ok((Vec::new(), Vec::new()));
    }
    // P15-T03: recall reads the checked-out branch plus 'main', drops a 'main' row that the
    // branch shadows for the same key, and refuses lapsed temporary memories by clock rather
    // than by waiting for the expiry sweep to have run.
    let branch = active_branch(tx, scope)?;
    let now_unix = Utc::now().timestamp();
    struct Row {
        memory: Recall,
        updated_at: String,
        pinned: bool,
        vector: Vec<f32>,
        recall_count: i64,
        useful_count: i64,
    }
    let raw = {
        let mut stmt = tx.prepare("SELECT m.id,m.scope,m.key,m.value,m.revision,c.evidence,m.updated_at,m.pinned,e.dimensions,e.vector,e.content_hash,COALESCE(e.recall_count,0),COALESCE(e.useful_count,0) FROM memories m JOIN candidates c ON c.id=m.candidate_id LEFT JOIN memory_embeddings e ON e.memory_id=m.id AND e.model=?1 WHERE m.status='active' AND m.branch IN ('main',?3) AND (m.expires_at IS NULL OR m.expires_at>?4) AND NOT EXISTS(SELECT 1 FROM memories b WHERE b.scope=m.scope AND b.key=m.key AND b.branch=?3 AND b.status='active' AND (b.expires_at IS NULL OR b.expires_at>?4) AND m.branch<>?3) AND (m.scope=?2 OR m.scope='global') AND (m.scope=?2 OR NOT EXISTS(SELECT 1 FROM memories p WHERE p.scope=?2 AND p.key=m.key AND p.status='active' AND p.branch IN ('main',?3))) ORDER BY m.id LIMIT 10000")?;
        let collected = stmt
            .query_map(
                params![crate::embeddings::MODEL, scope, branch, now_unix],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, i64>(7)?,
                        r.get::<_, Option<i64>>(8)?,
                        r.get::<_, Option<Vec<u8>>>(9)?,
                        r.get::<_, Option<String>>(10)?,
                        r.get::<_, i64>(11)?,
                        r.get::<_, i64>(12)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    };
    let mut rows = Vec::new();
    for (
        id,
        row_scope,
        key,
        value,
        revision,
        evidence,
        updated_at,
        pinned,
        dimensions,
        blob,
        stored_hash,
        recall_count,
        useful_count,
    ) in raw
    {
        if safety::sensitive(&value) {
            continue;
        }
        let content = format!("{key}\n{value}");
        let content_hash = safety::fingerprint(&content);
        // `dimensions` is whatever the row holds; a negative or oversized value must fail
        // the cache lookup, not wrap into a huge length.
        let stored_dimensions = dimensions.and_then(|value| usize::try_from(value).ok());
        let decoded = blob
            .as_deref()
            .zip(stored_dimensions)
            .and_then(|(bytes, dims)| crate::embeddings::decode(bytes, dims));
        let cache_hit = stored_hash.as_deref() == Some(&content_hash)
            && dimensions == Some(crate::embeddings::DIMENSIONS as i64)
            && decoded.is_some();
        let vector = match decoded {
            Some(cached) if cache_hit => cached,
            _ => crate::embeddings::embed(&content),
        };
        if !cache_hit && persist {
            tx.execute("INSERT INTO memory_embeddings(memory_id,model,dimensions,vector,content_hash,recall_count,useful_count,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(memory_id) DO UPDATE SET model=excluded.model,dimensions=excluded.dimensions,vector=excluded.vector,content_hash=excluded.content_hash,updated_at=excluded.updated_at",
                params![id,crate::embeddings::MODEL,crate::embeddings::DIMENSIONS as i64,crate::embeddings::encode(&vector),content_hash,recall_count,useful_count,now()])?;
        }
        rows.push(Row {
            memory: Recall {
                id,
                scope: row_scope,
                key,
                value,
                revision,
                evidence: serde_json::from_str(&evidence).unwrap_or(Value::Null),
            },
            updated_at,
            pinned: pinned != 0,
            vector,
            recall_count,
            useful_count,
        });
    }

    let mut lexical = HashMap::<String, usize>::new();
    if !fts.is_empty() {
        let mut stmt = tx.prepare("SELECT m.id FROM memory_fts JOIN memories m ON m.rowid=memory_fts.rowid WHERE memory_fts MATCH ?1 AND m.status='active' AND m.branch IN ('main',?3) AND (m.expires_at IS NULL OR m.expires_at>?4) AND NOT EXISTS(SELECT 1 FROM memories b WHERE b.scope=m.scope AND b.key=m.key AND b.branch=?3 AND b.status='active' AND (b.expires_at IS NULL OR b.expires_at>?4) AND m.branch<>?3) AND (m.scope=?2 OR m.scope='global') AND (m.scope=?2 OR NOT EXISTS(SELECT 1 FROM memories p WHERE p.scope=?2 AND p.key=m.key AND p.status='active' AND p.branch IN ('main',?3))) ORDER BY bm25(memory_fts),m.updated_at DESC LIMIT 20")?;
        for (rank, id) in stmt
            .query_map(params![fts, scope, branch, now_unix], |r| {
                r.get::<_, String>(0)
            })?
            .enumerate()
        {
            lexical.insert(id?, rank);
        }
    }
    let semantic = if semantic_enabled {
        let mut scored = rows
            .iter()
            .map(|row| {
                (
                    row.memory.id.clone(),
                    crate::embeddings::cosine(&query_vector, &row.vector),
                )
            })
            .filter(|(_, score)| *score > 0.01)
            .collect::<Vec<_>>();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        scored.truncate(20);
        scored.into_iter().collect::<HashMap<_, _>>()
    } else {
        HashMap::new()
    };
    let candidate_ids = lexical
        .keys()
        .chain(semantic.keys())
        .cloned()
        .collect::<HashSet<_>>();
    let now_at = Utc::now();
    struct Scores {
        lexical: f64,
        semantic: f64,
        scope: f64,
        recency: f64,
        usefulness: f64,
        total: f64,
    }
    let mut ranked = rows
        .into_iter()
        .filter(|row| candidate_ids.contains(&row.memory.id))
        .map(|row| {
            let lexical_score = lexical
                .get(&row.memory.id)
                .map(|rank| 2.0 / (1.0 + *rank as f64))
                .unwrap_or(0.0);
            let semantic_score = semantic
                .get(&row.memory.id)
                .copied()
                .unwrap_or(0.0)
                .max(0.0) as f64;
            let scope_score = if row.memory.scope == scope { 0.5 } else { 0.0 };
            let days = chrono::DateTime::parse_from_rfc3339(&row.updated_at)
                .map(|at| (now_at - at.with_timezone(&Utc)).num_days().max(0) as f64)
                .unwrap_or(365.0);
            let recency_score = 0.25 / (1.0 + days / 30.0);
            let useful_ratio =
                row.useful_count.max(0) as f64 / (1 + row.recall_count.max(0)) as f64;
            let usefulness_score = 0.5 * useful_ratio;
            let total =
                lexical_score + semantic_score + scope_score + recency_score + usefulness_score;
            (
                Scores {
                    lexical: lexical_score,
                    semantic: semantic_score,
                    scope: scope_score,
                    recency: recency_score,
                    usefulness: usefulness_score,
                    total,
                },
                row,
            )
        })
        .collect::<Vec<_>>();
    // Pinned profile entries sort ahead of the scored order and survive the rank cutoff. That
    // is an eligibility rule, not a hidden score: `total_score` in the receipt must stay the sum
    // of the parts printed beside it.
    ranked.sort_by(|a, b| {
        b.1.pinned
            .cmp(&a.1.pinned)
            .then_with(|| b.0.total.total_cmp(&a.0.total))
            .then_with(|| b.1.updated_at.cmp(&a.1.updated_at))
            .then_with(|| a.1.memory.id.cmp(&b.1.memory.id))
    });
    let mut out = Vec::new();
    let mut explained = Vec::new();
    let mut bytes = 0usize;
    for (index, (scores, row)) in ranked.into_iter().enumerate() {
        let size = serde_json::to_vec(&row.memory)?.len();
        // Order matters: the rank cutoff is decided before the payload ceiling, which is the
        // order recall itself applies them.
        let (decision, reason) = if index >= RECALL_RANK_LIMIT && !row.pinned {
            ("excluded", "rank_cutoff")
        } else if bytes + size > RECALL_BYTE_CEILING {
            // The byte ceiling still binds a pinned entry. A pin decides priority, not that the
            // context budget may be exceeded.
            ("excluded", "payload_ceiling")
        } else if row.pinned {
            ("included", "pinned_profile")
        } else {
            ("included", "ranked_and_fit")
        };
        explained.push(RecallCandidate {
            id: row.memory.id.clone(),
            scope: row.memory.scope.clone(),
            key: row.memory.key.clone(),
            revision: row.memory.revision,
            rank: index as i64,
            decision: decision.to_string(),
            reason: reason.to_string(),
            lexical_score: scores.lexical,
            semantic_score: scores.semantic,
            scope_score: scores.scope,
            recency_score: scores.recency,
            usefulness_score: scores.usefulness,
            total_score: scores.total,
            bytes: size as i64,
        });
        if decision == "included" {
            bytes += size;
            if persist {
                tx.execute(
                    "UPDATE memory_embeddings SET recall_count=recall_count+1 WHERE memory_id=?1",
                    [&row.memory.id],
                )?;
            }
            out.push(row.memory);
        }
    }
    Ok((out, explained))
}

/// The approval write itself, shared by `resolve` and by the rehearsal preview. The preview runs
/// it inside a transaction it then rolls back, so the preview cannot approve anything.
///
/// P15-T03: an approval lands on the scope's checked-out branch, so reviewing on a branch never
/// overwrites the value on 'main'. It also writes the decision timeline: the newly chosen value,
/// and the value it replaced kept beside it as superseded rather than erased.
// Every parameter here is one column of the approval write, and each one is read by name inside
// the function; bundling them into a struct would only move the same list one level away.
#[allow(clippy::too_many_arguments)]
fn apply_candidate(
    tx: &Transaction<'_>,
    id: &str,
    scope: &str,
    key: &str,
    value: &str,
    category: &str,
    expected: i64,
    branch: &str,
) -> Result<&'static str> {
    safety::validate_fact(key, value, category)?;
    let current: Option<(String, i64, String)> = tx
        .query_row(
            "SELECT id,revision,value FROM memories WHERE scope=?1 AND key=?2 AND branch=?3",
            params![scope, key, branch],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if current.as_ref().map(|m| m.1).unwrap_or(0) != expected {
        tx.execute(
            "UPDATE candidates SET status='conflict',resolved_at=?1 WHERE id=?2",
            params![now(), id],
        )?;
        return Ok("conflict");
    }
    let memory_id = current.as_ref().map(|m| m.0.clone()).unwrap_or_else(uid);
    let old = current.map(|m| m.2);
    let stamp = now();
    let group = decision_group(scope, key, branch);
    tx.execute("INSERT INTO memories(id,scope,key,value,branch,category,status,revision,candidate_id,conflict_group,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,'active',?7,?8,?9,?10,?10) ON CONFLICT(scope,key,branch) DO UPDATE SET value=excluded.value,category=excluded.category,status='active',revision=excluded.revision,candidate_id=excluded.candidate_id,conflict_group=excluded.conflict_group,updated_at=excluded.updated_at",params![memory_id,scope,key,value,branch,category,expected+1,id,group,stamp])?;
    tx.execute("INSERT INTO memory_revisions(id,memory_id,revision,action,old_value,new_value,candidate_id,created_at) VALUES(?1,?2,?3,'approve',?4,?5,?6,?7)",params![uid(),memory_id,expected+1,old,value,id,stamp])?;
    // A memory approved before this table existed has no timeline yet. Record the value being
    // replaced once, so a timeline never opens by implying the current value was always chosen.
    if let Some(previous) = &old {
        let seen: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM memory_decisions WHERE group_id=?1 LIMIT 1",
                [&group],
                |r| r.get(0),
            )
            .optional()?;
        if seen.is_none() {
            tx.execute("INSERT INTO memory_decisions(id,group_id,scope,key,branch,memory_id,candidate_id,value,state,reason,revision,decided_at) VALUES(?1,?2,?3,?4,?5,?6,NULL,?7,'superseded','replaced_by_newer',?8,?9)",
                params![uid(),group,scope,key,branch,memory_id,previous,expected,stamp])?;
        }
    }
    tx.execute("UPDATE memory_decisions SET state='superseded',reason='replaced_by_newer' WHERE group_id=?1 AND state='chosen'",[&group])?;
    tx.execute("INSERT INTO memory_decisions(id,group_id,scope,key,branch,memory_id,candidate_id,value,state,reason,revision,decided_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'chosen','approved',?9,?10)",
        params![uid(),group,scope,key,branch,memory_id,id,value,expected+1,stamp])?;
    tx.execute(
        "UPDATE candidates SET status='approved',resolved_at=?1 WHERE id=?2 AND status='pending'",
        params![stamp, id],
    )?;
    Ok("approved")
}

impl DbStore {
    /// Store a model-produced turn summary as review-only episodic evidence. It does not enter
    /// recall until a person approves the candidate.
    pub async fn save_compaction_candidate(
        &self,
        scope: String,
        request: String,
        step: String,
        summary: String,
    ) -> Result<String> {
        safety::validate_fact("turn_summary", &summary, "episodic")?;
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let source_id=format!("compaction:{step}");
            let fingerprint=crate::tools::content_hash(&summary);
            let stamp=now();
            tx.execute("INSERT INTO sources(id,scope,name,format,fingerprint,parser_version,content,warnings,created_at) VALUES(?1,?2,?3,'compaction',?4,'turn-compaction-v1',?5,'[]',?6) ON CONFLICT(id) DO NOTHING",
                params![source_id,scope,format!("Turn compaction {request}"),fingerprint,summary,stamp])?;
            // The expected revision is per branch: a candidate raised while a review branch is
            // checked out must be compared against that branch's row, not against 'main'.
            let branch=active_branch(&tx,&scope)?;
            let revision:i64=tx.query_row("SELECT revision FROM memories WHERE scope=?1 AND key='turn_summary' AND branch=?2",params![scope,branch],|r|r.get(0)).optional()?.unwrap_or(0);
            let candidate=uid();
            let evidence=json!({"source_id":source_id,"request_id":request,"step_id":step,"kind":"compaction","quote":summary});
            tx.execute("INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at) VALUES(?1,?2,'turn_summary',?3,'episodic',?4,?5,?6,'pending',?7,?8)",
                params![candidate,scope,summary,source_id,evidence.to_string(),revision,stamp,Utc::now().timestamp()+30*86400])?;
            tx.commit()?;
            Ok(candidate)
        }).await
    }
    /// Unfiltered candidate feed. Kept as the narrow entry point beside `candidate_feed`.
    #[allow(dead_code)]
    pub async fn candidates(&self, scope: String) -> Result<Value> {
        self.candidate_feed(scope, None, false, false).await
    }
    pub async fn candidate_feed(
        &self,
        scope: String,
        request: Option<String>,
        imports_only: bool,
        chat_only: bool,
    ) -> Result<Value> {
        self.run(move|c|{
            c.execute("UPDATE candidates SET status='expired',resolved_at=?1 WHERE status='pending' AND expires_at<=?2",params![now(),Utc::now().timestamp()])?;
            let mut stmt=c.prepare("SELECT c.id,c.scope,c.key,c.value,c.category,c.evidence,c.expected_revision,m.value,c.source_id,c.created_at FROM candidates c LEFT JOIN memories m ON m.scope=c.scope AND m.key=c.key WHERE c.scope=?1 AND c.status='pending' ORDER BY c.created_at LIMIT 500")?;
            let raw=stmt.query_map([scope],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,i64>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,String>(8)?,r.get::<_,String>(9)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut items=Vec::new();
            for (id,row_scope,key,value,category,evidence_text,expected_revision,old_value,source_id,created_at) in raw {
                let evidence=serde_json::from_str::<Value>(&evidence_text).unwrap_or(Value::Null);
                let request_id=evidence.get("request_id").and_then(Value::as_str).map(str::to_string).or_else(||source_id.strip_prefix("chat:").map(str::to_string));
                if request.as_ref().is_some_and(|wanted|request_id.as_ref()!=Some(wanted)){continue;}
                if imports_only && request_id.is_some(){continue;}
                if chat_only && request_id.is_none(){continue;}
                let priority=evidence.get("priority").and_then(Value::as_str).unwrap_or("normal");
                items.push(json!({"id":id,"scope":row_scope,"key":key,"value":value,"category":category,"evidence":evidence,"expected_revision":expected_revision,"old_value":old_value,"request_id":request_id,"priority":priority,"created_at":created_at}));
            }
            items.sort_by(|a,b|(b["priority"]==json!("high")).cmp(&(a["priority"]==json!("high"))).then_with(||a["created_at"].as_str().cmp(&b["created_at"].as_str())).then_with(||a["id"].as_str().cmp(&b["id"].as_str())));
            items.truncate(100);Ok(json!({"candidates":items}))
        }).await
    }
    pub async fn edit_candidate(
        &self,
        id: String,
        scope: String,
        new_value: String,
    ) -> Result<String> {
        let value = new_value.trim().to_string();
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row:Option<(String,String,String,String,i64,String)>=tx.query_row("SELECT key,category,status,source_id,expires_at,evidence FROM candidates WHERE id=?1 AND scope=?2",params![id,scope],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
            let Some((key,category,status,source_id,expiry,evidence_text))=row else{return Ok("not_found".into())};
            if status!="pending"{return Ok("already_resolved".into());}
            if expiry<=Utc::now().timestamp(){tx.execute("UPDATE candidates SET status='expired',resolved_at=?1 WHERE id=?2",params![now(),id])?;tx.commit()?;return Ok("expired".into());}
            safety::validate_fact(&key,&value,&category)?;
            let duplicate:Option<i64>=tx.query_row("SELECT 1 FROM candidates WHERE scope=?1 AND key=?2 AND value=?3 AND source_id=?4 AND id<>?5",params![scope,key,value,source_id,id],|r|r.get(0)).optional()?;
            if duplicate.is_some(){return Ok("conflict".into());}
            let mut evidence=serde_json::from_str::<Value>(&evidence_text)?;
            let Some(object)=evidence.as_object_mut() else {bail!("candidate evidence must be an object")};
            object.insert("edited".into(),json!(true));
            tx.execute("UPDATE candidates SET value=?1,evidence=?2 WHERE id=?3 AND scope=?4 AND status='pending'",params![value,evidence.to_string(),id,scope])?;
            tx.commit()?;Ok("edited".into())
        }).await
    }
    pub async fn resolve(&self, id: String, scope: String, confirm: bool) -> Result<String> {
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row:Option<(String,String,String,i64,String,i64)>=tx.query_row("SELECT key,value,category,expected_revision,status,expires_at FROM candidates WHERE id=?1 AND scope=?2",params![id,scope],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
            let Some((key,value,category,expected,status,expiry))=row else{return Ok("not_found".into())};
            if status!="pending" {return Ok("already_resolved".into());}
            if expiry<=Utc::now().timestamp(){tx.execute("UPDATE candidates SET status='expired',resolved_at=?1 WHERE id=?2",params![now(),id])?;tx.commit()?;return Ok("expired".into());}
            let branch=active_branch(&tx,&scope)?;
            if !confirm {
                let stamp=now();
                tx.execute("UPDATE candidates SET status='rejected',resolved_at=?1 WHERE id=?2 AND status='pending'",params![stamp,id])?;
                // A rejected value stays on the timeline as considered, so the record shows what
                // was turned down instead of only what was kept.
                tx.execute("INSERT INTO memory_decisions(id,group_id,scope,key,branch,memory_id,candidate_id,value,state,reason,revision,decided_at) VALUES(?1,?2,?3,?4,?5,NULL,?6,?7,'considered','rejected',?8,?9)",
                    params![uid(),decision_group(&scope,&key,&branch),scope,key,branch,id,value,expected,stamp])?;
                tx.commit()?;return Ok("rejected".into());
            }
            let outcome=apply_candidate(&tx,&id,&scope,&key,&value,&category,expected,&branch)?;
            tx.commit()?;Ok(outcome.into())
        }).await
    }
    /// Hybrid local recall: union lexical and vector top-20s, then rerank deterministically.
    /// The final context payload retains the original 6,000-byte hard ceiling.
    /// Kept as the narrow payload-only entry point: the turn uses `recall_explained`, while the
    /// recall evaluation and the memory tests drive this one.
    #[allow(dead_code)]
    pub async fn recall(&self, scope: String, prompt: String) -> Result<Vec<Recall>> {
        self.recall_with_strategy(scope, prompt, crate::embeddings::strategy_from_env())
            .await
    }

    /// P15-T01: the strategy is an explicit argument so tests and the recall evaluation can
    /// compare arms without mutating process environment, which would race other tests.
    #[allow(dead_code)]
    pub async fn recall_with_strategy(
        &self,
        scope: String,
        prompt: String,
        strategy: crate::embeddings::Strategy,
    ) -> Result<Vec<Recall>> {
        Ok(self.recall_explained(scope, prompt, strategy).await?.0)
    }

    /// P15-T02: the same recall, plus the per-candidate scores and decisions behind it. Callers
    /// that only need the payload go through `recall_with_strategy`.
    pub async fn recall_explained(
        &self,
        scope: String,
        prompt: String,
        strategy: crate::embeddings::Strategy,
    ) -> Result<(Vec<Recall>, Vec<RecallCandidate>)> {
        // Checked before opening a write transaction: a prompt no arm can match must not take
        // the database lock at all.
        if no_arm_can_match(
            &safety::fts_query(&prompt),
            &crate::embeddings::embed(&prompt),
            strategy,
        ) {
            return Ok((Vec::new(), Vec::new()));
        }
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let ranked = rank_in_tx(&tx, &scope, &prompt, strategy, true)?;
            tx.commit()?;
            Ok(ranked)
        })
        .await
    }

    /// P15-T02 rehearsal: what this scope's retrieval would return for `prompt`, and what it
    /// would return if `candidate` were approved. The approval is applied inside a transaction
    /// that is always rolled back, so the shipped ranking answers the question and nothing is
    /// written — including the recall counters, which a preview must not move.
    ///
    /// This is a retrieval difference only. It does not claim the answer would change.
    pub async fn preview_retrieval(
        &self,
        scope: String,
        prompt: String,
        candidate: Option<String>,
    ) -> Result<Value> {
        let strategy = crate::embeddings::strategy_from_env();
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (_, before) = rank_in_tx(&tx, &scope, &prompt, strategy, false)?;
            let mut applied = "none";
            if let Some(id) = candidate.as_deref() {
                let row:Option<(String,String,String,i64,String,i64)>=tx.query_row("SELECT key,value,category,expected_revision,status,expires_at FROM candidates WHERE id=?1 AND scope=?2",params![id,scope],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
                applied = match row {
                    None => "not_found",
                    Some((_, _, _, _, status, _)) if status != "pending" => "already_resolved",
                    Some((_, _, _, _, _, expiry)) if expiry <= Utc::now().timestamp() => "expired",
                    Some((key, value, category, expected, _, _)) => {
                        let branch = active_branch(&tx, &scope)?;
                        apply_candidate(
                            &tx, id, &scope, &key, &value, &category, expected, &branch,
                        )?
                    }
                };
            }
            let after = if applied == "approved" {
                rank_in_tx(&tx, &scope, &prompt, strategy, false)?.1
            } else {
                before.clone()
            };
            // Always: a preview that could commit would be an approval wearing a preview's name.
            tx.rollback()?;
            let included = |rows: &[RecallCandidate]| {
                rows.iter()
                    .filter(|row| row.decision == "included")
                    .map(|row| row.id.clone())
                    .collect::<HashSet<_>>()
            };
            let before_included = included(&before);
            let after_included = included(&after);
            let describe = |rows: &[RecallCandidate], ids: &HashSet<String>| {
                rows.iter()
                    .filter(|row| ids.contains(&row.id))
                    .map(|row| json!({"memory_id":row.id,"key":row.key,"scope":row.scope,"revision":row.revision,"total_score":row.total_score,"rank":row.rank}))
                    .collect::<Vec<_>>()
            };
            let added = after_included
                .difference(&before_included)
                .cloned()
                .collect::<HashSet<_>>();
            let removed = before_included
                .difference(&after_included)
                .cloned()
                .collect::<HashSet<_>>();
            Ok(json!({
                "scope":scope,
                "strategy":strategy_label(strategy),
                "candidate_id":candidate,
                "candidate_state":applied,
                "rehearsed":applied=="approved",
                "before":before.iter().map(RecallCandidate::as_value).collect::<Vec<_>>(),
                "after":after.iter().map(RecallCandidate::as_value).collect::<Vec<_>>(),
                "added":describe(&after,&added),
                "removed":describe(&before,&removed),
                "note":"Deterministic re-run of the shipped retrieval ranking over the memories stored right now. It shows which memories retrieval would include, not how the model would answer, and nothing was saved."
            }))
        })
        .await
    }

    /// Persist the retrieval explanation for one turn. Write-once, like the context receipt it
    /// accompanies: a second call for the same request is ignored rather than allowed to rewrite
    /// history.
    pub async fn save_retrieval_receipt(
        &self,
        request: String,
        scope: String,
        strategy: crate::embeddings::Strategy,
        prompt: String,
        budget_bytes: i64,
        candidates: Vec<RecallCandidate>,
    ) -> Result<bool> {
        let fingerprint = safety::fingerprint(&prompt);
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let included_bytes: i64 = candidates
                .iter()
                .filter(|row| row.decision == "included")
                .map(|row| row.bytes)
                .sum();
            let included = candidates
                .iter()
                .filter(|row| row.decision == "included")
                .count() as i64;
            let inserted = tx.execute("INSERT INTO retrieval_receipts(request_id,scope,strategy,embedding_model,prompt_fingerprint,budget_bytes,included_bytes,considered,included,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(request_id) DO NOTHING",
                params![request,scope,strategy_label(strategy),crate::embeddings::MODEL,fingerprint,budget_bytes,included_bytes,candidates.len() as i64,included,now()])?;
            if inserted == 0 {
                tx.commit()?;
                return Ok(false);
            }
            for row in &candidates {
                tx.execute("INSERT INTO retrieval_candidates(request_id,memory_id,scope,key,revision,rank,decision,reason,lexical_score,semantic_score,scope_score,recency_score,usefulness_score,total_score,bytes) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                    params![request,row.id,row.scope,row.key,row.revision,row.rank,row.decision,row.reason,row.lexical_score,row.semantic_score,row.scope_score,row.recency_score,row.usefulness_score,row.total_score,row.bytes])?;
            }
            tx.commit()?;
            Ok(true)
        })
        .await
    }

    /// The persisted retrieval explanation for one turn, exactly as it was written.
    pub async fn retrieval_receipt(&self, request: String) -> Result<Option<Value>> {
        self.read(move |c| {
            let header:Option<ReceiptHeader>=c.query_row("SELECT scope,strategy,embedding_model,prompt_fingerprint,budget_bytes,included_bytes,considered,included,created_at FROM retrieval_receipts WHERE request_id=?1",[&request],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).optional()?;
            let Some((scope,strategy,model,fingerprint,budget_bytes,included_bytes,considered,included,created_at))=header else{return Ok(None)};
            let mut stmt=c.prepare("SELECT memory_id,scope,key,revision,rank,decision,reason,lexical_score,semantic_score,scope_score,recency_score,usefulness_score,total_score,bytes FROM retrieval_candidates WHERE request_id=?1 ORDER BY rank")?;
            let rows=stmt.query_map([&request],|r|Ok(json!({
                "memory_id":r.get::<_,String>(0)?,"scope":r.get::<_,String>(1)?,"key":r.get::<_,String>(2)?,"revision":r.get::<_,i64>(3)?,
                "rank":r.get::<_,i64>(4)?,"decision":r.get::<_,String>(5)?,"reason":r.get::<_,String>(6)?,
                "lexical_score":r.get::<_,f64>(7)?,"semantic_score":r.get::<_,f64>(8)?,"scope_score":r.get::<_,f64>(9)?,
                "recency_score":r.get::<_,f64>(10)?,"usefulness_score":r.get::<_,f64>(11)?,"total_score":r.get::<_,f64>(12)?,"bytes":r.get::<_,i64>(13)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Some(json!({
                "format_version":1,
                "request_id":request,
                "scope":scope,
                "strategy":strategy,
                "embedding_model":model,
                "prompt_fingerprint":fingerprint,
                "budget_bytes":budget_bytes,
                "included_bytes":included_bytes,
                "considered":considered,
                "included":included,
                "created_at":created_at,
                "candidates":rows,
                "note":"Recorded retrieval evidence for this turn: which memories were ranked, which were sent and why the rest were not. It is not a claim about how the model used them."
            })))
        })
        .await
    }

    /// P15-T03: everything a reviewer needs to govern one scope — branches, pinned and temporary
    /// entries, conflict groups and duplicate suggestions. Read-only: duplicates are reported as
    /// suggestions because SQLite `lower()` is ASCII-only, so an automatic merge would be a guess.
    pub async fn memory_governance(&self, scope: String) -> Result<Value> {
        self.read(move |c| {
            let active = active_branch(c, &scope)?;
            let mut stmt = c.prepare("SELECT m.id,m.key,m.value,m.branch,m.category,m.status,m.revision,m.pinned,m.expires_at,m.conflict_group,m.updated_at,COALESCE(e.recall_count,0),(SELECT count(*) FROM memory_feedback f WHERE f.memory_id=m.id AND f.verdict='useful'),(SELECT count(*) FROM memory_feedback f WHERE f.memory_id=m.id AND f.verdict='not_useful') FROM memories m LEFT JOIN memory_embeddings e ON e.memory_id=m.id WHERE m.scope=?1 ORDER BY m.pinned DESC,m.key,m.branch LIMIT 500")?;
            let entries = stmt
                .query_map([&scope], |r| {
                    Ok(json!({
                        "id":r.get::<_,String>(0)?,"key":r.get::<_,String>(1)?,"value":r.get::<_,String>(2)?,
                        "branch":r.get::<_,String>(3)?,"category":r.get::<_,String>(4)?,"status":r.get::<_,String>(5)?,
                        "revision":r.get::<_,i64>(6)?,"pinned":r.get::<_,i64>(7)?==1,"expires_at":r.get::<_,Option<i64>>(8)?,
                        "conflict_group":r.get::<_,Option<String>>(9)?,"updated_at":r.get::<_,String>(10)?,
                        "recall_count":r.get::<_,i64>(11)?,"useful":r.get::<_,i64>(12)?,"not_useful":r.get::<_,i64>(13)?}))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut branches = vec![json!({"branch":"main","active":active=="main","note":"reviewed default","created_at":Value::Null})];
            let mut stmt = c.prepare("SELECT branch,active,note,created_at FROM memory_branches WHERE scope=?1 ORDER BY branch LIMIT 100")?;
            for row in stmt.query_map([&scope], |r| {
                Ok(json!({"branch":r.get::<_,String>(0)?,"active":r.get::<_,i64>(1)?==1,"note":r.get::<_,Option<String>>(2)?,"created_at":r.get::<_,String>(3)?}))
            })? {
                branches.push(row?);
            }
            let mut stmt = c.prepare("SELECT dedup_key,count(*),group_concat(id,' '),group_concat(key,' ') FROM memories WHERE scope=?1 AND status='active' GROUP BY dedup_key HAVING count(*)>1 ORDER BY count(*) DESC LIMIT 50")?;
            let duplicates = stmt
                .query_map([&scope], |r| {
                    Ok(json!({
                        "dedup_key":r.get::<_,String>(0)?,
                        "count":r.get::<_,i64>(1)?,
                        "memory_ids":r.get::<_,String>(2)?.split(' ').map(str::to_string).collect::<Vec<_>>(),
                        "keys":r.get::<_,String>(3)?.split(' ').map(str::to_string).collect::<Vec<_>>()}))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let temporary = entries
                .iter()
                .filter(|row| !row["expires_at"].is_null())
                .cloned()
                .collect::<Vec<_>>();
            let pinned = entries
                .iter()
                .filter(|row| row["pinned"] == json!(true))
                .cloned()
                .collect::<Vec<_>>();
            Ok(json!({
                "format_version":1,
                "scope":scope,
                "active_branch":active,
                "branches":branches,
                "entries":entries,
                "pinned":pinned,
                "temporary":temporary,
                "duplicate_suggestions":duplicates,
                "note":"Stored governance state for this scope. Duplicate groups are suggestions for review, never merged automatically."
            }))
        })
        .await
    }

    /// The review history of one memory: value revisions, the decisions around them and the
    /// usefulness verdicts recorded against each revision, all as stored.
    pub async fn memory_timeline(&self, scope: String, memory_id: String) -> Result<Option<Value>> {
        self.read(move |c| {
            let head: Option<GovernedMemory> = c
                .query_row("SELECT key,value,branch,status,revision,pinned,expires_at,conflict_group,created_at,updated_at FROM memories WHERE id=?1 AND scope=?2",
                    params![memory_id, scope],
                    |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)))
                .optional()?;
            let Some((key,value,branch,status,revision,pinned,expires_at,conflict_group,created_at,updated_at)) = head else {
                return Ok(None);
            };
            let group = conflict_group
                .clone()
                .unwrap_or_else(|| decision_group(&scope, &key, &branch));
            let mut stmt = c.prepare("SELECT revision,action,old_value,new_value,detail,created_at FROM memory_revisions WHERE memory_id=?1 ORDER BY created_at,revision LIMIT 500")?;
            let revisions = stmt
                .query_map([&memory_id], |r| {
                    Ok(json!({"revision":r.get::<_,i64>(0)?,"action":r.get::<_,String>(1)?,"old_value":r.get::<_,Option<String>>(2)?,
                        "new_value":r.get::<_,String>(3)?,"detail":r.get::<_,Option<String>>(4)?,"created_at":r.get::<_,String>(5)?}))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut stmt = c.prepare("SELECT value,state,reason,revision,branch,decided_at FROM memory_decisions WHERE group_id=?1 ORDER BY decided_at,id LIMIT 500")?;
            let decisions = stmt
                .query_map([&group], |r| {
                    Ok(json!({"value":r.get::<_,String>(0)?,"state":r.get::<_,String>(1)?,"reason":r.get::<_,String>(2)?,
                        "revision":r.get::<_,i64>(3)?,"branch":r.get::<_,String>(4)?,"decided_at":r.get::<_,String>(5)?}))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut stmt = c.prepare("SELECT revision,verdict,request_id,note,created_at FROM memory_feedback WHERE memory_id=?1 ORDER BY created_at LIMIT 500")?;
            let feedback = stmt
                .query_map([&memory_id], |r| {
                    Ok(json!({"revision":r.get::<_,i64>(0)?,"verdict":r.get::<_,String>(1)?,"request_id":r.get::<_,Option<String>>(2)?,
                        "note":r.get::<_,Option<String>>(3)?,"created_at":r.get::<_,String>(4)?}))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Some(json!({
                "format_version":1,
                "id":memory_id,
                "scope":scope,
                "key":key,
                "value":value,
                "branch":branch,
                "status":status,
                "revision":revision,
                "pinned":pinned==1,
                "expires_at":expires_at,
                "conflict_group":group,
                "created_at":created_at,
                "updated_at":updated_at,
                "revisions":revisions,
                "decisions":decisions,
                "feedback":feedback,
                "note":"Stored review history for this memory. Superseded and expired values are retained, not deleted."
            })))
        })
        .await
    }

    /// One audited governance act on one memory. Every arm writes a `memory_revisions` row, so a
    /// pin, an expiry, a merge or a usefulness verdict is recoverable history rather than a silent
    /// state change.
    // The optional arguments are the per-action inputs (expiry clock, merge target, note, request
    // id). They stay explicit so a caller cannot silently omit the one its action needs.
    #[allow(clippy::too_many_arguments)]
    pub async fn govern_memory(
        &self,
        scope: String,
        memory_id: String,
        action: String,
        expires_at: Option<i64>,
        target: Option<String>,
        note: Option<String>,
        request: Option<String>,
    ) -> Result<String> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row: Option<(String, String, String, String, i64, String)> = tx
                .query_row("SELECT key,value,branch,status,revision,candidate_id FROM memories WHERE id=?1 AND scope=?2",
                    params![memory_id, scope],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
                .optional()?;
            let Some((key, value, branch, status, revision, candidate_id)) = row else {
                return Ok("not_found".to_string());
            };
            let feedback_only = action == "useful" || action == "not_useful";
            if !feedback_only && status != "active" {
                return Ok("not_active".to_string());
            }
            let stamp = now();
            let group = decision_group(&scope, &key, &branch);
            let outcome = match action.as_str() {
                "pin" | "unpin" => {
                    let pinned = i64::from(action == "pin");
                    tx.execute("UPDATE memories SET pinned=?1,updated_at=?2 WHERE id=?3", params![pinned, stamp, memory_id])?;
                    tx.execute("INSERT INTO memory_revisions(id,memory_id,revision,action,old_value,new_value,candidate_id,created_at,detail) VALUES(?1,?2,?3,?4,NULL,?5,?6,?7,?8)",
                        params![uid(),memory_id,revision,action,value,candidate_id,stamp,note.clone().unwrap_or_else(||"owner decision".to_string())])?;
                    action.clone()
                }
                "expire" => {
                    tx.execute("UPDATE memories SET expires_at=?1,updated_at=?2 WHERE id=?3", params![expires_at, stamp, memory_id])?;
                    let detail = match expires_at {
                        Some(at) => format!("scheduled_at={at}"),
                        None => "cleared".to_string(),
                    };
                    tx.execute("INSERT INTO memory_revisions(id,memory_id,revision,action,old_value,new_value,candidate_id,created_at,detail) VALUES(?1,?2,?3,'expire',NULL,?4,?5,?6,?7)",
                        params![uid(),memory_id,revision,value,candidate_id,stamp,detail])?;
                    if expires_at.is_some() { "expiry_scheduled".to_string() } else { "expiry_cleared".to_string() }
                }
                "merge" => {
                    let Some(duplicate) = target.clone() else {
                        return Ok("missing_target".to_string());
                    };
                    if duplicate == memory_id {
                        return Ok("same_memory".to_string());
                    }
                    let other: Option<(String, String, String, i64, String)> = tx
                        .query_row("SELECT key,value,branch,revision,candidate_id FROM memories WHERE id=?1 AND scope=?2 AND status='active'",
                            params![duplicate, scope],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
                        .optional()?;
                    let Some((other_key, other_value, other_branch, other_revision, other_candidate)) = other else {
                        return Ok("target_not_found".to_string());
                    };
                    // The absorbed row keeps its value and its revision chain; it stops being
                    // recalled and says who absorbed it.
                    tx.execute("UPDATE memories SET status='superseded',conflict_group=?1,updated_at=?2 WHERE id=?3", params![group, stamp, duplicate])?;
                    tx.execute("UPDATE memories SET conflict_group=?1,updated_at=?2 WHERE id=?3", params![group, stamp, memory_id])?;
                    tx.execute("INSERT INTO memory_revisions(id,memory_id,revision,action,old_value,new_value,candidate_id,created_at,detail) VALUES(?1,?2,?3,'merge',NULL,?4,?5,?6,?7)",
                        params![uid(),memory_id,revision,value,candidate_id,stamp,format!("absorbed={duplicate}")])?;
                    tx.execute("INSERT INTO memory_revisions(id,memory_id,revision,action,old_value,new_value,candidate_id,created_at,detail) VALUES(?1,?2,?3,'merge',NULL,?4,?5,?6,?7)",
                        params![uid(),duplicate,other_revision,other_value,other_candidate,stamp,format!("merged_into={memory_id}")])?;
                    tx.execute("INSERT INTO memory_decisions(id,group_id,scope,key,branch,memory_id,candidate_id,value,state,reason,revision,decided_at) VALUES(?1,?2,?3,?4,?5,?6,NULL,?7,'superseded','duplicate_merged',?8,?9)",
                        params![uid(),group,scope,other_key,other_branch,duplicate,other_value,other_revision,stamp])?;
                    "merged".to_string()
                }
                "useful" | "not_useful" => {
                    // UNIQUE treats NULL request ids as distinct in SQLite, so the same verdict
                    // given twice outside a recorded turn would insert twice. `IS` compares NULL
                    // to NULL as equal, so the check covers both cases.
                    let already: i64 = tx.query_row("SELECT count(*) FROM memory_feedback WHERE memory_id=?1 AND revision=?2 AND request_id IS ?3 AND verdict=?4",
                        params![memory_id,revision,request,action],|r|r.get(0))?;
                    let inserted = if already > 0 {
                        0
                    } else {
                        tx.execute("INSERT INTO memory_feedback(id,memory_id,revision,request_id,verdict,note,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(memory_id,revision,request_id,verdict) DO NOTHING",
                            params![uid(),memory_id,revision,request,action,note,stamp])?
                    };
                    if inserted == 0 {
                        "duplicate_feedback".to_string()
                    } else {
                        if action == "useful" {
                            // The usefulness ranking term reads memory_embeddings.useful_count, so
                            // the counter advances from the same act that recorded the verdict.
                            tx.execute("UPDATE memory_embeddings SET useful_count=useful_count+1 WHERE memory_id=?1", [&memory_id])?;
                        }
                        "recorded".to_string()
                    }
                }
                _ => "unsupported_action".to_string(),
            };
            tx.commit()?;
            Ok(outcome)
        })
        .await
    }

    /// Create and/or check out a memory branch for one scope. Approvals then land on that branch
    /// and recall prefers it over 'main' for the same key, which is what lets a value be revised
    /// under review without overwriting the reviewed one.
    pub async fn checkout_memory_branch(
        &self,
        scope: String,
        branch: String,
        note: Option<String>,
        activate: bool,
    ) -> Result<Value> {
        let branch = branch.trim().to_string();
        if branch.is_empty() || branch.chars().count() > 60 {
            bail!("branch must be 1..=60 characters");
        }
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            if branch != "main" {
                tx.execute("INSERT INTO memory_branches(scope,branch,active,note,created_at) VALUES(?1,?2,0,?3,?4) ON CONFLICT(scope,branch) DO UPDATE SET note=COALESCE(excluded.note,memory_branches.note)",
                    params![scope, branch, note, stamp])?;
            }
            if activate {
                // Absence of an active row means 'main', so checking out 'main' clears rather
                // than records a selection.
                tx.execute("UPDATE memory_branches SET active=0 WHERE scope=?1", [&scope])?;
                if branch != "main" {
                    tx.execute("UPDATE memory_branches SET active=1 WHERE scope=?1 AND branch=?2", params![scope, branch])?;
                }
            }
            let active = active_branch(&tx, &scope)?;
            tx.commit()?;
            Ok(json!({"scope":scope,"branch":branch,"active_branch":active}))
        })
        .await
    }

    /// Flip lapsed temporary memories to 'expired' and record why. Recall already filters on
    /// `expires_at` by clock, so this sweep decides what the review surfaces show, never whether
    /// a lapsed memory could still be retrieved.
    pub async fn lapse_expired_memories(&self) -> Result<i64> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            let due = {
                let mut stmt = tx.prepare("SELECT id,scope,key,value,branch,revision,candidate_id FROM memories WHERE status='active' AND expires_at IS NOT NULL AND expires_at<=?1 ORDER BY id LIMIT 500")?;
                let collected = stmt
                    .query_map([Utc::now().timestamp()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, String>(6)?,
                    ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                collected
            };
            for (id, row_scope, key, value, branch, revision, candidate) in &due {
                tx.execute("UPDATE memories SET status='expired',updated_at=?1 WHERE id=?2", params![stamp, id])?;
                tx.execute("INSERT INTO memory_revisions(id,memory_id,revision,action,old_value,new_value,candidate_id,created_at,detail) VALUES(?1,?2,?3,'expire',NULL,?4,?5,?6,'lapsed')",
                    params![uid(), id, revision, value, candidate, stamp])?;
                tx.execute("INSERT INTO memory_decisions(id,group_id,scope,key,branch,memory_id,candidate_id,value,state,reason,revision,decided_at) VALUES(?1,?2,?3,?4,?5,?6,NULL,?7,'superseded','expired',?8,?9)",
                    params![uid(),decision_group(row_scope,key,branch),row_scope,key,branch,id,value,revision,stamp])?;
            }
            let count = due.len() as i64;
            tx.commit()?;
            Ok(count)
        })
        .await
    }
}
