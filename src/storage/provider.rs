//! P12-T02 seam 5: provider-call accounting and the generation/activity feeds.
//! Spend reservation, call completion and the streaming event appenders move here
//! byte for byte from `src/storage.rs`. The child module still uses the private
//! `DbStore::run`/`read` helpers, so no visibility was widened.

use super::{now, uid, DbStore};
use crate::memory_agents::{ModelUsage, SpendLimits};
use anyhow::{bail, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

impl DbStore {
    /// Atomically reserve one provider dispatch after checking persisted per-turn and UTC-day
    /// request, token, and cost totals. Refusals are durable and are never sent to the provider.
    pub async fn reserve_provider_call(
        &self,
        request_id: Option<String>,
        kind: String,
        model: String,
        limits: SpendLimits,
    ) -> Result<String> {
        let call_id = uid();
        let returned = call_id.clone();
        let day_start = format!("{}T00:00:00+00:00", Utc::now().date_naive());
        let decision = self.run(move |c| {
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let totals = |request: Option<&str>| -> Result<(i64,i64,i64,i64)> {
                let (where_sql, request_param) = if request.is_some() {
                    ("created_at>=?1 AND request_id=?2", request)
                } else {
                    ("created_at>=?1", None)
                };
                let sql = format!("SELECT count(*),COALESCE(sum(prompt_tokens+completion_tokens),0),COALESCE(sum(cost_microusd),0),COALESCE(sum(CASE WHEN state IN ('complete','failed') AND (usage_status!='reported') THEN 1 ELSE 0 END),0) FROM provider_calls WHERE state IN ('reserved','complete','failed') AND {where_sql}");
                if let Some(value)=request_param {
                    Ok(tx.query_row(&sql, params![day_start,value], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?)
                } else {
                    Ok(tx.query_row(&sql, params![day_start], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?)
                }
            };
            let day=totals(None)?;
            let turn=if let Some(request)=request_id.as_deref(){totals(Some(request))?}else{(0,0,0,0)};
            let reason = if turn.0 >= limits.requests_per_turn as i64 { Some("turn_request_limit") }
                else if day.0 >= limits.requests_per_day as i64 { Some("daily_request_limit") }
                else if turn.3 > 0 && (limits.tokens_per_turn.is_some() || limits.cost_microusd_per_turn.is_some()) { Some("turn_usage_unavailable") }
                else if day.3 > 0 && (limits.tokens_per_day.is_some() || limits.cost_microusd_per_day.is_some()) { Some("daily_usage_unavailable") }
                else if limits.tokens_per_turn.is_some_and(|v| turn.1 >= v as i64) { Some("turn_token_limit") }
                else if limits.tokens_per_day.is_some_and(|v| day.1 >= v as i64) { Some("daily_token_limit") }
                else if limits.cost_microusd_per_turn.is_some_and(|v| turn.2 >= v as i64) { Some("turn_cost_limit") }
                else if limits.cost_microusd_per_day.is_some_and(|v| day.2 >= v as i64) { Some("daily_cost_limit") }
                else if (limits.cost_microusd_per_turn.is_some() || limits.cost_microusd_per_day.is_some()) && !limits.has_pricing() { Some("cost_pricing_unavailable") }
                else { None };
            let stamp=now();
            if let Some(reason)=reason {
                tx.execute("INSERT INTO provider_calls(call_id,request_id,kind,model,state,usage_status,reason,created_at,finished_at) VALUES(?1,?2,?3,?4,'refused','unavailable',?5,?6,?6)",params![call_id,request_id,kind,model,reason,stamp])?;
                if let Some(request)=request_id.as_deref() {
                    if let Some(session)=tx.query_row("SELECT session_id FROM chat_receipts WHERE request_id=?1",[request],|r|r.get::<_,String>(0)).optional()? {
                        tx.execute(crate::agentic_sql::EVENT,params![request,session,None::<String>,"provider_spend_refused",json!({"reason":reason}).to_string(),stamp])?;
                    }
                }
                tx.commit()?;
                return Ok(Err(reason.to_string()));
            }
            tx.execute("INSERT INTO provider_calls(call_id,request_id,kind,model,state,usage_status,created_at) VALUES(?1,?2,?3,?4,'reserved','pending',?5)",params![call_id,request_id,kind,model,stamp])?;
            tx.commit()?;
            Ok(Ok(()))
        }).await?;
        match decision {
            Ok(()) => Ok(returned),
            Err(reason) => bail!("provider spend refused: {reason}"),
        }
    }
    pub async fn finish_provider_call(
        &self,
        call_id: String,
        usage: ModelUsage,
        limits: SpendLimits,
        failed_reason: Option<String>,
    ) -> Result<()> {
        let prompt = usage.prompt_tokens.map(|v| v as i64);
        let completion = usage.completion_tokens.map(|v| v as i64);
        let reported = prompt.is_some() && completion.is_some();
        let cost = if reported {
            limits.cost_for(&usage).map(|v| v as i64)
        } else {
            None
        };
        let failed = failed_reason.is_some();
        let reason = failed_reason
            .or_else(|| usage.unavailable_reason.clone())
            .or_else(|| {
                if cost.is_none() {
                    Some("cost_pricing_unavailable".into())
                } else {
                    None
                }
            });
        self.run(move|c|{
            if c.execute("UPDATE provider_calls SET state=?2,prompt_tokens=?3,completion_tokens=?4,cost_microusd=?5,usage_status=?6,reason=?7,finished_at=?8 WHERE call_id=?1 AND state='reserved'",params![call_id,if failed{"failed"}else{"complete"},prompt,completion,cost,if reported{"reported"}else{"unavailable"},reason,now()])?!=1 { bail!("provider call reservation is not active"); }
            Ok(())
        }).await
    }
    /// Durable assistant generation chunks. The producer writes these rows before transport.
    pub async fn append_generation(
        &self,
        request_id: String,
        session_id: String,
        state: String,
        content: String,
        error_code: Option<String>,
    ) -> Result<i64> {
        self.run(move|c|{
            c.execute("INSERT INTO generation_events(request_id,session_id,state,content,error_code,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![request_id,session_id,state,content,error_code,now()])?;
            Ok(c.last_insert_rowid())
        }).await
    }
    pub async fn generation_since(&self, session_id: String, after_seq: i64) -> Result<Value> {
        self.run(move|c|{
            let mut stmt=c.prepare("SELECT seq,request_id,state,content,error_code,created_at FROM generation_events WHERE session_id=?1 AND seq>?2 ORDER BY seq LIMIT 200")?;
            let rows=stmt.query_map(params![session_id,after_seq],|r|Ok(json!({
                "seq":r.get::<_,i64>(0)?,"request_id":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"content":r.get::<_,String>(3)?,"error_code":r.get::<_,Option<String>>(4)?,"created_at":r.get::<_,String>(5)?
            })))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let next=rows.last().and_then(|e|e["seq"].as_i64()).unwrap_or(after_seq);
            Ok(json!({"events":rows,"next_after_seq":next}))
        }).await
    }
    /// The session's activity feed after `after_seq`, capped at 200 rows by `EVENTS_AFTER`.
    /// `next_after_seq` is the cursor to send back; it only moves when rows were returned, so a
    /// poll that finds nothing cannot skip an event that commits a moment later.
    pub async fn activity_since(&self, session_id: String, after_seq: i64) -> Result<Value> {
        self.read(move|c|{
            let mut stmt=c.prepare(crate::agentic_sql::EVENTS_AFTER)?;
            let rows=stmt.query_map(params![session_id,after_seq],|r|Ok(json!({
                "seq":r.get::<_,i64>(0)?,"request_id":r.get::<_,String>(1)?,"step_id":r.get::<_,Option<String>>(2)?,
                "kind":r.get::<_,String>(3)?,
                "payload":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(Value::Null),
                "created_at":r.get::<_,String>(5)?,
            })))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let next=rows.last().and_then(|e|e["seq"].as_i64()).unwrap_or(after_seq);
            Ok(json!({"events":rows,"next_after_seq":next}))
        }).await
    }
}
