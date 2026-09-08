use anyhow::{bail, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use uuid::Uuid;
use crate::{ingest::Event, safety};

pub fn now() -> String { Utc::now().to_rfc3339() }
pub fn uid() -> String { Uuid::new_v4().to_string() }
#[derive(Clone)]
pub struct DbStore { conn: Arc<Mutex<Connection>>, permits: Arc<Semaphore> }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proposal { pub key:String, pub value:String, pub category:String, pub evidence_id:String, pub quote:String }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recall { pub id:String, pub scope:String, pub key:String, pub value:String, pub revision:i64, pub evidence:Value }
pub struct Job { pub id:String, pub scope:String, pub source_id:String, pub events:Vec<Event>, pub attempts:i64 }

impl DbStore {
    pub fn init(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Inspect first: rejecting a legacy DB must not change its journal mode.
        let version:i64 = conn.query_row("PRAGMA user_version",[],|r|r.get(0))?;
        if version == 0 {
            let existing:i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",[],|r|r.get(0))?;
            if existing != 0 { bail!("legacy or unknown database: use scripts/migrate_legacy.py into a NEW database"); }
        } else if version != 1 { bail!("unsupported schema version {version}"); }
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
        if version == 0 { conn.execute_batch(include_str!("../migrations/001_core.sql"))?; }
        // Single-process service: persisted work is retried after a process restart.
        conn.execute("UPDATE jobs SET status='pending' WHERE status='running'",[])?;
        conn.execute("UPDATE messages SET status='failed' WHERE status='pending'",[])?;
        Ok(Self{conn:Arc::new(Mutex::new(conn)),permits:Arc::new(Semaphore::new(32))})
    }
    pub async fn run<T,F>(&self, f:F) -> Result<T>
    where T:Send+'static, F:FnOnce(&mut Connection)->Result<T>+Send+'static {
        let permit = self.permits.clone().try_acquire_owned().map_err(|_|anyhow::anyhow!("database queue full"))?;
        let shared = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let _permit=permit;
            let mut conn=shared.lock().map_err(|_|anyhow::anyhow!("database lock poisoned"))?;
            f(&mut conn)
        }).await?
    }
    pub async fn settings(&self) -> Result<Value> {
        self.run(|c| {
            let mut map=serde_json::Map::new();
            for role in ["main","extraction"] {
                let value:Option<String>=c.query_row("SELECT value FROM settings WHERE key=?1",[format!("model.{role}")],|r|r.get(0)).optional()?;
                map.insert(role.into(),json!(value.unwrap_or_default()));
            }
            Ok(Value::Object(map))
        }).await
    }
    pub async fn set_settings(&self, data:std::collections::BTreeMap<String,String>) -> Result<()> {
        for (role,value) in &data { if !["main","extraction"].contains(&role.as_str()) || value.len()>128 || value.chars().any(char::is_control) { bail!("invalid model setting"); } }
        self.run(move|c|{ let tx=c.transaction()?; for (role,value) in data {tx.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![format!("model.{role}"),value.trim()])?;} tx.commit()?; Ok(()) }).await
    }
    pub async fn role_model(&self, role:&str, default:&str) -> Result<String> {
        let key=format!("model.{role}"); let default=default.to_string();
        self.run(move|c|{let v:Option<String>=c.query_row("SELECT value FROM settings WHERE key=?1",[key],|r|r.get(0)).optional()?; Ok(v.filter(|s|!s.is_empty()).unwrap_or(default))}).await
    }
    pub async fn begin_chat(&self, session:String, scope:String, request:String, prompt:String) -> Result<Vec<Event>> {
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute("INSERT INTO sessions(id,scope,created_at) VALUES(?1,?2,?3) ON CONFLICT(id) DO NOTHING",params![session,scope,now()])?;
            let actual:String=tx.query_row("SELECT scope FROM sessions WHERE id=?1",[&session],|r|r.get(0))?;
            if actual!=scope {bail!("session belongs to a different scope");}
            let mut events={
                let mut stmt=tx.prepare("SELECT id,role,content FROM (SELECT seq,id,role,content FROM messages WHERE session_id=?1 AND status='complete' ORDER BY seq DESC LIMIT 20) ORDER BY seq")?;
                let rows=stmt.query_map([&session],|r|Ok(Event{id:r.get(0)?,role:r.get(1)?,content:r.get(2)?}))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            // Successful chat writes complete user/assistant turns atomically.
            while events.iter().map(|e|e.content.len()).sum::<usize>()>24_000 && events.len()>=2 {events.drain(..2);}
            tx.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?1,?2,'user',?3,'pending',?4)",params![request,session,prompt,now()])?;
            events.push(Event{id:request,role:"user".into(),content:prompt});
            tx.commit()?; Ok(events)
        }).await
    }
    pub async fn fail_chat(&self, request:String) -> Result<()> {
        self.run(move|c|{c.execute("UPDATE messages SET status='failed' WHERE id=?1 AND status='pending'",[request])?;Ok(())}).await
    }
    pub async fn complete_chat(&self,session:String,scope:String,request:String,prompt:String,answer:String)->Result<String>{
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed=tx.execute("UPDATE messages SET status='complete' WHERE id=?1 AND session_id=?2 AND status='pending'",params![request,session])?;
            if changed!=1 {bail!("request not pending");}
            tx.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?1,?2,'assistant',?3,'complete',?4)",params![uid(),session,answer,now()])?;
            let queued:i64=tx.query_row("SELECT count(*) FROM jobs WHERE status IN ('pending','running')",[],|r|r.get(0))?;
            if queued>=1000 {bail!("extraction queue is full");}
            let job=uid(); let events=vec![Event{id:request.clone(),role:"user".into(),content:prompt}];
            tx.execute("INSERT INTO jobs(id,job_key,scope,source_id,payload,status,available_at,created_at) VALUES(?1,?2,?3,?4,?5,'pending',?6,?7)",params![job,format!("chat:{request}"),scope,format!("chat:{request}"),serde_json::to_string(&events)?,Utc::now().timestamp(),now()])?;
            tx.commit()?;Ok(job)
        }).await
    }
    pub async fn history(&self,session:String)->Result<Value>{
        self.run(move|c|{
            let scope:Option<String>=c.query_row("SELECT scope FROM sessions WHERE id=?1",[&session],|r|r.get(0)).optional()?;
            let mut stmt=c.prepare("SELECT id,role,content,status FROM (SELECT seq,id,role,content,status FROM messages WHERE session_id=?1 ORDER BY seq DESC LIMIT 100) ORDER BY seq")?;
            let rows=stmt.query_map([session],|r|Ok(json!({"id":r.get::<_,String>(0)?,"role":r.get::<_,String>(1)?,"content":r.get::<_,String>(2)?,"status":r.get::<_,String>(3)?})))?;
            Ok(json!({"scope":scope,"messages":rows.collect::<rusqlite::Result<Vec<_>>>()?}))
        }).await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn ingest(&self,scope:String,name:String,format:String,content:String,fingerprint:String,warnings:Vec<String>,chunks:Vec<Vec<Event>>)->Result<Value>{
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
    pub async fn claim_job(&self)->Result<Option<Job>>{
        self.run(|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row:Option<(String,String,String,String,i64)>=tx.query_row("SELECT id,scope,source_id,payload,attempts FROM jobs WHERE status='pending' AND available_at<=?1 ORDER BY created_at,id LIMIT 1",[Utc::now().timestamp()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
            let Some((id,scope,source_id,payload,attempts))=row else{return Ok(None)};
            let events=serde_json::from_str(&payload)?;
            tx.execute("UPDATE jobs SET status='running',attempts=attempts+1 WHERE id=?1 AND status='pending'",[&id])?;
            tx.commit()?;Ok(Some(Job{id,scope,source_id,events,attempts:attempts+1}))
        }).await
    }
    pub async fn finish_job(&self,job:Job,proposals:Vec<Proposal>)->Result<usize>{
        // Validate the entire result before storing ANY proposals.
        if proposals.len()>10 {bail!("too many extraction proposals");}
        for p in &proposals {
            safety::validate_fact(&p.key,&p.value,&p.category)?;
            if p.quote.trim().is_empty() || p.quote.chars().count()>1000 || !job.events.iter().any(|e| e.id==p.evidence_id && e.role=="user" && e.content.contains(&p.quote)) {bail!("proposal has invalid user evidence");}
        }
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;let mut count=0;
            for p in proposals {
                let revision:i64=tx.query_row("SELECT revision FROM memories WHERE scope=?1 AND key=?2",params![job.scope,p.key],|r|r.get(0)).optional()?.unwrap_or(0);
                let evidence=json!({"source_id":job.source_id,"event_id":p.evidence_id,"quote":p.quote});
                count+=tx.execute("INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending',?9,?10) ON CONFLICT(scope,key,value,source_id) DO NOTHING",params![uid(),job.scope,p.key,p.value,p.category,job.source_id,evidence.to_string(),revision,now(),Utc::now().timestamp()+30*86400])?;
            }
            tx.execute("UPDATE jobs SET status='done',last_error=NULL WHERE id=?1",[job.id])?;
            tx.commit()?;Ok(count)
        }).await
    }
    pub async fn fail_job(&self,id:String,attempts:i64)->Result<()> {
        self.run(move|c|{c.execute("UPDATE jobs SET status=?1,available_at=?2,last_error='extraction or validation failed; inspect provider configuration and retry' WHERE id=?3",params![if attempts>=3{"failed"}else{"pending"},Utc::now().timestamp()+30*attempts,id])?;Ok(())}).await
    }
    pub async fn retry_job(&self,id:String)->Result<bool>{
        self.run(move|c|Ok(c.execute("UPDATE jobs SET status='pending',attempts=0,available_at=?1,last_error=NULL WHERE id=?2 AND status='failed'",params![Utc::now().timestamp(),id])?==1)).await
    }
    pub async fn candidates(&self,scope:String)->Result<Value>{
        self.run(move|c|{
            c.execute("UPDATE candidates SET status='expired',resolved_at=?1 WHERE status='pending' AND expires_at<=?2",params![now(),Utc::now().timestamp()])?;
            let mut stmt=c.prepare("SELECT c.id,c.scope,c.key,c.value,c.category,c.evidence,c.expected_revision,m.value FROM candidates c LEFT JOIN memories m ON m.scope=c.scope AND m.key=c.key WHERE c.scope=?1 AND c.status='pending' ORDER BY c.created_at LIMIT 100")?;
            let rows=stmt.query_map([scope],|r|Ok(json!({"id":r.get::<_,String>(0)?,"scope":r.get::<_,String>(1)?,"key":r.get::<_,String>(2)?,"value":r.get::<_,String>(3)?,"category":r.get::<_,String>(4)?,"evidence":serde_json::from_str::<Value>(&r.get::<_,String>(5)?).unwrap_or(Value::Null),"expected_revision":r.get::<_,i64>(6)?,"old_value":r.get::<_,Option<String>>(7)?})))?;
            Ok(json!({"candidates":rows.collect::<rusqlite::Result<Vec<_>>>()?}))
        }).await
    }
    pub async fn resolve(&self,id:String,scope:String,confirm:bool)->Result<String>{
        self.run(move|c|{
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row:Option<(String,String,String,i64,String,i64)>=tx.query_row("SELECT key,value,category,expected_revision,status,expires_at FROM candidates WHERE id=?1 AND scope=?2",params![id,scope],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
            let Some((key,value,category,expected,status,expiry))=row else{return Ok("not_found".into())};
            if status!="pending" {return Ok("already_resolved".into());}
            if expiry<=Utc::now().timestamp(){tx.execute("UPDATE candidates SET status='expired',resolved_at=?1 WHERE id=?2",params![now(),id])?;tx.commit()?;return Ok("expired".into());}
            if !confirm {tx.execute("UPDATE candidates SET status='rejected',resolved_at=?1 WHERE id=?2 AND status='pending'",params![now(),id])?;tx.commit()?;return Ok("rejected".into());}
            safety::validate_fact(&key,&value,&category)?;
            let current:Option<(String,i64,String)>=tx.query_row("SELECT id,revision,value FROM memories WHERE scope=?1 AND key=?2",params![scope,key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if current.as_ref().map(|m|m.1).unwrap_or(0)!=expected {
                tx.execute("UPDATE candidates SET status='conflict',resolved_at=?1 WHERE id=?2",params![now(),id])?;tx.commit()?;return Ok("conflict".into());
            }
            let memory_id=current.as_ref().map(|m|m.0.clone()).unwrap_or_else(uid);
            let old=current.map(|m|m.2);let stamp=now();
            tx.execute("INSERT INTO memories(id,scope,key,value,category,status,revision,candidate_id,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,'active',?6,?7,?8,?8) ON CONFLICT(scope,key) DO UPDATE SET value=excluded.value,category=excluded.category,status='active',revision=excluded.revision,candidate_id=excluded.candidate_id,updated_at=excluded.updated_at",params![memory_id,scope,key,value,category,expected+1,id,stamp])?;
            tx.execute("INSERT INTO memory_revisions(id,memory_id,revision,action,old_value,new_value,candidate_id,created_at) VALUES(?1,?2,?3,'approve',?4,?5,?6,?7)",params![uid(),memory_id,expected+1,old,value,id,stamp])?;
            tx.execute("UPDATE candidates SET status='approved',resolved_at=?1 WHERE id=?2 AND status='pending'",params![stamp,id])?;
            tx.commit()?;Ok("approved".into())
        }).await
    }
    pub async fn recall(&self,scope:String,prompt:String)->Result<Vec<Recall>>{
        let query=safety::fts_query(&prompt);if query.is_empty(){return Ok(Vec::new());}
        self.run(move|c|{
            let mut stmt=c.prepare("SELECT m.id,m.scope,m.key,m.value,m.revision,c.evidence FROM memory_fts JOIN memories m ON m.rowid=memory_fts.rowid JOIN candidates c ON c.id=m.candidate_id WHERE memory_fts MATCH ?1 AND m.status='active' AND (m.scope=?2 OR m.scope='global') AND (m.scope=?2 OR NOT EXISTS(SELECT 1 FROM memories p WHERE p.scope=?2 AND p.key=m.key AND p.status='active')) ORDER BY (m.scope=?2) DESC,bm25(memory_fts),m.updated_at DESC LIMIT 6")?;
            let rows=stmt.query_map(params![query,scope],|r|Ok(Recall{id:r.get(0)?,scope:r.get(1)?,key:r.get(2)?,value:r.get(3)?,revision:r.get(4)?,evidence:serde_json::from_str(&r.get::<_,String>(5)?).unwrap_or(Value::Null)}))?;
            let mut out=Vec::new();let mut bytes=0;
            for item in rows {let item=item?;let length=serde_json::to_string(&item)?.len();if bytes+length<=6000 && !safety::sensitive(&item.value){bytes+=length;out.push(item);}}
            Ok(out)
        }).await
    }
    pub async fn stats(&self)->Result<Value>{
        self.run(|c|{
            let active:i64=c.query_row("SELECT count(*) FROM memories WHERE status='active'",[],|r|r.get(0))?;
            let pending:i64=c.query_row("SELECT count(*) FROM candidates WHERE status='pending' AND expires_at>?1",[Utc::now().timestamp()],|r|r.get(0))?;
            let sources:i64=c.query_row("SELECT count(*) FROM sources",[],|r|r.get(0))?;
            let queued:i64=c.query_row("SELECT count(*) FROM jobs WHERE status IN ('pending','running')",[],|r|r.get(0))?;
            let failed:i64=c.query_row("SELECT count(*) FROM jobs WHERE status='failed'",[],|r|r.get(0))?;
            Ok(json!({"active_memories":active,"pending_confirmations":pending,"sources_stored":sources,"queued_jobs":queued,"failed_jobs":failed}))
        }).await
    }
    pub async fn jobs(&self)->Result<Value>{
        self.run(|c|{let mut stmt=c.prepare("SELECT id,scope,source_id,status,attempts,last_error FROM jobs ORDER BY created_at DESC LIMIT 100")?;
            let rows=stmt.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"scope":r.get::<_,String>(1)?,"source_id":r.get::<_,String>(2)?,"status":r.get::<_,String>(3)?,"attempts":r.get::<_,i64>(4)?,"error":r.get::<_,Option<String>>(5)?})))?;
            Ok(json!({"jobs":rows.collect::<rusqlite::Result<Vec<_>>>()?}))
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test] async fn import_is_idempotent_and_not_active() {
        let db=DbStore::init(":memory:").unwrap();let events=vec![Event{id:"event-1:part-0".into(),role:"user".into(),content:"I prefer Rust".into()}];
        let a=db.ingest("global".into(),"test".into(),"claude".into(),"safe source".into(),"digest".into(),vec![],vec![events.clone()]).await.unwrap();
        assert_eq!(a["duplicate"],false);
        let b=db.ingest("global".into(),"test".into(),"claude".into(),"safe source".into(),"digest".into(),vec![],vec![events]).await.unwrap();assert_eq!(b["duplicate"],true);
        let job=db.claim_job().await.unwrap().unwrap();db.finish_job(job,vec![Proposal{key:"language".into(),value:"Rust".into(),category:"preference".into(),evidence_id:"event-1:part-0".into(),quote:"I prefer Rust".into()}]).await.unwrap();
        assert_eq!(db.stats().await.unwrap()["active_memories"],0);
        let list=db.candidates("global".into()).await.unwrap();let id=list["candidates"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(db.resolve(id.clone(),"wrong-scope".into(),true).await.unwrap(),"not_found");
        assert_eq!(db.resolve(id.clone(),"global".into(),true).await.unwrap(),"approved");
        assert_eq!(db.resolve(id,"global".into(),false).await.unwrap(),"already_resolved");
        assert_eq!(db.recall("global".into(),"Rust:".into()).await.unwrap().len(),1);
    }
}
