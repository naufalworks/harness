//! P12-T02 seam 6: turn projections and file-change history. The plan/step
//! projections, the per-turn change list and the file-change lookup plus revert
//! recorder move here byte for byte from `src/storage.rs`. The child module still
//! uses the private `DbStore::run`/`read` helpers, so no visibility was widened.

use super::{now, plan_rows, verification_projection, DbStore, PREVIEW_BYTES};
use anyhow::Result;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

impl DbStore {
    /// The stored plan, for the `plan_updated` event and the UI's plan panel.
    pub async fn plan(&self, session_id: String) -> Result<Value> {
        self.read(move |c| plan_rows(c, &session_id)).await
    }

    // ---- P1-T12 read side: what the UI polls between turns ----------------------------
    // These only read rows the loop already committed. Nothing here recomputes a summary or
    // re-renders a diff, so the UI can never show a version of the turn the record disagrees with.
    /// Every step of one turn in `seq` order. `input_preview`/`output_preview` are the first
    /// 2 KB of the stored JSON as text (`SQL substr`), so a long tool output or a whole message
    /// array cannot blow up a poll; `previews_capped` says when that cut happened, because a
    /// chopped JSON string that pretends to be complete is worse than no preview at all.
    /// `summary` is the tool's own phrase, read back from the finished step's output; a step that
    /// is still running has none yet, and its `tool_started` activity event carries it instead.
    pub async fn turn_steps(&self, request_id: String) -> Result<Value> {
        self.run(move|c|{
            let mut stmt=c.prepare(crate::agentic_sql::STEPS_LIST)?;
            let rows=stmt.query_map([&request_id],|r|{
                let input:Option<String>=r.get(6)?;
                let output:Option<String>=r.get(7)?;
                let capped=[input.as_deref(),output.as_deref()].iter().flatten().any(|p|p.len()>=PREVIEW_BYTES);
                let summary=output.as_deref().and_then(|p|serde_json::from_str::<Value>(p).ok())
                    .and_then(|v|v.get("summary").and_then(Value::as_str).map(str::to_string));
                Ok(json!({
                    "id":r.get::<_,String>(0)?,"seq":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?,
                    "status":r.get::<_,String>(3)?,"tool_name":r.get::<_,Option<String>>(4)?,
                    "tool_call_id":r.get::<_,Option<String>>(5)?,"summary":summary,
                    "input_preview":input,"output_preview":output,"previews_capped":capped,
                    "output_bytes":r.get::<_,i64>(8)?,"truncated":r.get::<_,i64>(9)?==1,
                    "tokens_in":r.get::<_,Option<i64>>(10)?,"tokens_out":r.get::<_,Option<i64>>(11)?,
                    "error_code":r.get::<_,Option<String>>(12)?,
                    "started_at":r.get::<_,String>(13)?,"finished_at":r.get::<_,Option<String>>(14)?,
                }))
            })?.collect::<rusqlite::Result<Vec<_>>>()?;
            let verification=c.query_row(crate::agentic_sql::VERIFICATION_LATEST,[&request_id],|r|Ok(verification_projection(
                r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,
                r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,i64>(5)?==1,
            ))).optional()?;
            Ok(json!({"steps":rows,"verification":verification}))
        }).await
    }
    /// The file changes one turn applied, with the diff the tool produced. `applied` is the row's
    /// own word: the loop writes it when the file was already written, never in advance.
    pub async fn turn_changes(&self, request_id: String) -> Result<Value> {
        self.run(move|c|{
            let mut stmt=c.prepare(crate::agentic_sql::FILE_CHANGES_LIST)?;
            let rows=stmt.query_map([request_id],|r|Ok(json!({
                "id":r.get::<_,String>(0)?,"step_id":r.get::<_,String>(1)?,"path":r.get::<_,String>(2)?,
                "action":r.get::<_,String>(3)?,"before_hash":r.get::<_,Option<String>>(4)?,
                "after_hash":r.get::<_,Option<String>>(5)?,"diff":r.get::<_,Option<String>>(6)?,
                "applied":r.get::<_,i64>(7)?==1,"reverted_at":r.get::<_,Option<String>>(8)?,
                "created_at":r.get::<_,String>(9)?,
            })))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({"changes":rows}))
        }).await
    }
    /// One recorded change by id, with the scope and session of the turn that made it. P2-T03's
    /// revert resolves the project root from this row and never from the caller, so a revert
    /// cannot be pointed at another project's files.
    pub async fn file_change(&self, id: String) -> Result<Option<Value>> {
        self.run(move|c|{
            Ok(c.query_row(crate::agentic_sql::FILE_CHANGE_GET,[id],|r|Ok(json!({
                "id":r.get::<_,String>(0)?,"request_id":r.get::<_,String>(1)?,"step_id":r.get::<_,String>(2)?,
                "path":r.get::<_,String>(3)?,"action":r.get::<_,String>(4)?,"before_hash":r.get::<_,Option<String>>(5)?,
                "after_hash":r.get::<_,Option<String>>(6)?,"diff":r.get::<_,Option<String>>(7)?,
                "applied":r.get::<_,i64>(8)?==1,"reverted_at":r.get::<_,Option<String>>(9)?,
                "scope":r.get::<_,String>(10)?,"session_id":r.get::<_,String>(11)?,
            }))).optional()?)
        }).await
    }
    /// Record an undo that already happened on disk, together with the activity event that
    /// announces it, in one transaction. `false` means the row was already reverted and nothing
    /// was written twice — which is what a double-clicked Revert has to look like.
    pub async fn record_revert(
        &self,
        id: String,
        request: String,
        session: String,
        step: String,
        path: String,
    ) -> Result<bool> {
        self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stamp = now();
            if tx.execute(crate::agentic_sql::FILE_CHANGE_REVERTED, params![id, stamp])? != 1 {
                return Ok(false);
            }
            tx.execute(
                crate::agentic_sql::EVENT,
                params![
                    request,
                    session,
                    step,
                    "file_reverted",
                    json!({"change_id":id,"path":path}).to_string(),
                    stamp
                ],
            )?;
            tx.commit()?;
            Ok(true)
        })
        .await
    }
}
