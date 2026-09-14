//! P12-T02 seam 2: the extraction queue repository. Source ingestion, job claim,
//! completion, failure backoff, retry and the job listing move here byte for byte
//! from `src/storage.rs`. The child module still reaches `DbStore::run`/`read`, so
//! no connection-pool or field visibility was widened for the split.

use super::{now, uid, DbStore, Job, Proposal};
use crate::{ingest::Event, safety};
use anyhow::{bail, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

impl DbStore {
    #[allow(clippy::too_many_arguments)]
    pub async fn ingest(
        &self,
        scope: String,
        name: String,
        format: String,
        content: String,
        fingerprint: String,
        warnings: Vec<String>,
        chunks: Vec<Vec<Event>>,
    ) -> Result<Value> {
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let existing:Option<String>=tx.query_row("SELECT id FROM sources WHERE scope=?1 AND fingerprint=?2 AND parser_version=?3",params![scope,fingerprint,crate::ingest::PARSER_VERSION],|r|r.get(0)).optional()?;
            if let Some(id)=existing {return Ok(json!({"source_id":id,"duplicate":true,"chunks_queued":0}));}
            let queued:i64=tx.query_row("SELECT count(*) FROM jobs WHERE status IN ('pending','running')",[],|r|r.get(0))?;
            if queued+chunks.len() as i64>1000 {bail!("extraction queue is full");}
            let id=uid();
            tx.execute("INSERT INTO sources(id,scope,name,format,fingerprint,parser_version,content,warnings,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![id,scope,name,format,fingerprint,crate::ingest::PARSER_VERSION,content,serde_json::to_string(&warnings)?,now()])?;
            for (i,chunk) in chunks.iter().enumerate(){
                tx.execute("INSERT INTO jobs(id,job_key,scope,source_id,payload,status,available_at,created_at) VALUES(?1,?2,?3,?4,?5,'pending',?6,?7)",params![uid(),format!("{id}:chunk:{i}"),scope,id,serde_json::to_string(chunk)?,Utc::now().timestamp(),now()])?;
            }
            let result=json!({"source_id":id,"duplicate":false,"chunks_queued":chunks.len(),"warnings":warnings});
            tx.commit()?;Ok(result)
        }).await
    }
    pub async fn claim_job(&self) -> Result<Option<Job>> {
        self.flush_recording_outbox().await?;
        self.run(|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row:Option<(String,String,String,String,i64)>=tx.query_row("SELECT id,scope,source_id,payload,attempts FROM jobs WHERE status='pending' AND available_at<=?1 ORDER BY created_at,id LIMIT 1",[Utc::now().timestamp()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
            let Some((id,scope,source_id,payload,attempts))=row else{return Ok(None)};
            let events=serde_json::from_str(&payload)?;
            tx.execute("UPDATE jobs SET status='running',attempts=attempts+1 WHERE id=?1 AND status='pending'",[&id])?;
            tx.commit()?;Ok(Some(Job{id,scope,source_id,events,attempts:attempts+1}))
        }).await
    }
    pub async fn finish_job(&self, job: Job, proposals: Vec<Proposal>) -> Result<usize> {
        // Validate the entire result before storing ANY proposals.
        if proposals.len() > 10 {
            bail!("too many extraction proposals");
        }
        for p in &proposals {
            safety::validate_fact(&p.key, &p.value, &p.category)?;
            if p.priority
                .as_deref()
                .is_some_and(|value| !["normal", "high"].contains(&value))
            {
                bail!("proposal has invalid priority");
            }
            if p.quote.trim().is_empty()
                || p.quote.chars().count() > 1000
                || !job.events.iter().any(|e| {
                    e.id == p.evidence_id && e.role == "user" && e.content.contains(&p.quote)
                })
            {
                bail!("proposal has invalid user evidence");
            }
        }
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;let mut count=0;
            for p in proposals {
                let revision:i64=tx.query_row("SELECT revision FROM memories WHERE scope=?1 AND key=?2",params![job.scope,p.key],|r|r.get(0)).optional()?.unwrap_or(0);
                let correction=safety::is_correction(&p.quote);
                let evidence=json!({"source_id":job.source_id,"event_id":p.evidence_id,"quote":p.quote,"priority":if correction{"high"}else{"normal"},"correction":correction});
                count+=tx.execute("INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending',?9,?10) ON CONFLICT(scope,key,value,source_id) DO NOTHING",params![uid(),job.scope,p.key,p.value,p.category,job.source_id,evidence.to_string(),revision,now(),Utc::now().timestamp()+30*86400])?;
            }
            tx.execute("UPDATE jobs SET status='done',last_error=NULL WHERE id=?1",[job.id])?;
            tx.commit()?;Ok(count)
        }).await
    }
    pub async fn fail_job(&self, id: String, attempts: i64) -> Result<()> {
        self.run(move|c|{c.execute("UPDATE jobs SET status=?1,available_at=?2,last_error='extraction or validation failed; inspect provider configuration and retry' WHERE id=?3",params![if attempts>=3{"failed"}else{"pending"},Utc::now().timestamp()+30*attempts,id])?;Ok(())}).await
    }
    pub async fn retry_job(&self, id: String) -> Result<bool> {
        self.run(move|c|Ok(c.execute("UPDATE jobs SET status='pending',attempts=0,available_at=?1,last_error=NULL WHERE id=?2 AND status='failed'",params![Utc::now().timestamp(),id])?==1)).await
    }
    pub async fn jobs(&self) -> Result<Value> {
        self.read(|c|{let mut stmt=c.prepare("SELECT id,scope,source_id,status,attempts,last_error FROM jobs ORDER BY created_at DESC LIMIT 100")?;
            let rows=stmt.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"scope":r.get::<_,String>(1)?,"source_id":r.get::<_,String>(2)?,"status":r.get::<_,String>(3)?,"attempts":r.get::<_,i64>(4)?,"error":r.get::<_,Option<String>>(5)?})))?;
            Ok(json!({"jobs":rows.collect::<rusqlite::Result<Vec<_>>>()?}))
        }).await
    }
}
