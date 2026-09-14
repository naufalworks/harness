//! P12-T02 seam 3: the memory candidate repository. Compaction candidates, the
//! candidate feed, candidate edits, approve/reject resolution and hybrid recall
//! move here byte for byte from `src/storage.rs`. The child module still uses the
//! private `DbStore::run`/`read` helpers, so no visibility was widened.

use super::{now, uid, DbStore, Recall};
use crate::safety;
use anyhow::{bail, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

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
            let revision:i64=tx.query_row("SELECT revision FROM memories WHERE scope=?1 AND key='turn_summary'",[&scope],|r|r.get(0)).optional()?.unwrap_or(0);
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
    /// Hybrid local recall: union lexical and vector top-20s, then rerank deterministically.
    /// The final context payload retains the original 6,000-byte hard ceiling.
    pub async fn recall(&self, scope: String, prompt: String) -> Result<Vec<Recall>> {
        let fts = safety::fts_query(&prompt);
        let query_vector = crate::embeddings::embed(&prompt);
        if fts.is_empty() && query_vector.iter().all(|value| *value == 0.0) {
            return Ok(Vec::new());
        }
        self.run(move|c|{
            struct Row { memory:Recall, updated_at:String, vector:Vec<f32>, recall_count:i64, useful_count:i64 }
            let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let raw={
                let mut stmt=tx.prepare("SELECT m.id,m.scope,m.key,m.value,m.revision,c.evidence,m.updated_at,e.dimensions,e.vector,e.content_hash,COALESCE(e.recall_count,0),COALESCE(e.useful_count,0) FROM memories m JOIN candidates c ON c.id=m.candidate_id LEFT JOIN memory_embeddings e ON e.memory_id=m.id AND e.model=?1 WHERE m.status='active' AND (m.scope=?2 OR m.scope='global') AND (m.scope=?2 OR NOT EXISTS(SELECT 1 FROM memories p WHERE p.scope=?2 AND p.key=m.key AND p.status='active')) ORDER BY m.id LIMIT 10000")?;
                let collected=stmt.query_map(params![crate::embeddings::MODEL,scope],|r|Ok((
                    r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,
                    r.get::<_,Option<i64>>(7)?,r.get::<_,Option<Vec<u8>>>(8)?,r.get::<_,Option<String>>(9)?,r.get::<_,i64>(10)?,r.get::<_,i64>(11)?
                )))?.collect::<rusqlite::Result<Vec<_>>>()?;
                collected
            };
            let mut rows=Vec::new();
            for (id,row_scope,key,value,revision,evidence,updated_at,dimensions,blob,stored_hash,recall_count,useful_count) in raw {
                if safety::sensitive(&value){continue;}
                let content=format!("{key}\n{value}");
                let content_hash=safety::fingerprint(&content);
                // `dimensions` is whatever the row holds; a negative or oversized value must fail
                // the cache lookup, not wrap into a huge length.
                let stored_dimensions=dimensions.and_then(|value|usize::try_from(value).ok());
                let decoded=blob.as_deref().zip(stored_dimensions).and_then(|(bytes,dims)|crate::embeddings::decode(bytes,dims));
                let cache_hit=stored_hash.as_deref()==Some(&content_hash) && dimensions==Some(crate::embeddings::DIMENSIONS as i64) && decoded.is_some();
                let vector=match decoded{Some(cached) if cache_hit=>cached,_=>crate::embeddings::embed(&content)};
                if !cache_hit {
                    tx.execute("INSERT INTO memory_embeddings(memory_id,model,dimensions,vector,content_hash,recall_count,useful_count,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(memory_id) DO UPDATE SET model=excluded.model,dimensions=excluded.dimensions,vector=excluded.vector,content_hash=excluded.content_hash,updated_at=excluded.updated_at",
                        params![id,crate::embeddings::MODEL,crate::embeddings::DIMENSIONS as i64,crate::embeddings::encode(&vector),content_hash,recall_count,useful_count,now()])?;
                }
                rows.push(Row{memory:Recall{id,scope:row_scope,key,value,revision,evidence:serde_json::from_str(&evidence).unwrap_or(Value::Null)},updated_at,vector,recall_count,useful_count});
            }

            let mut lexical=HashMap::<String,usize>::new();
            if !fts.is_empty(){
                let mut stmt=tx.prepare("SELECT m.id FROM memory_fts JOIN memories m ON m.rowid=memory_fts.rowid WHERE memory_fts MATCH ?1 AND m.status='active' AND (m.scope=?2 OR m.scope='global') AND (m.scope=?2 OR NOT EXISTS(SELECT 1 FROM memories p WHERE p.scope=?2 AND p.key=m.key AND p.status='active')) ORDER BY bm25(memory_fts),m.updated_at DESC LIMIT 20")?;
                for (rank,id) in stmt.query_map(params![fts,scope],|r|r.get::<_,String>(0))?.enumerate(){lexical.insert(id?,rank);}
            }
            let mut semantic=rows.iter().map(|row|(row.memory.id.clone(),crate::embeddings::cosine(&query_vector,&row.vector))).filter(|(_,score)|*score>0.01).collect::<Vec<_>>();
            semantic.sort_by(|a,b|b.1.total_cmp(&a.1).then_with(||a.0.cmp(&b.0)));semantic.truncate(20);
            let semantic=semantic.into_iter().collect::<HashMap<_,_>>();
            let candidate_ids=lexical.keys().chain(semantic.keys()).cloned().collect::<HashSet<_>>();
            let now_at=Utc::now();
            let mut ranked=rows.into_iter().filter(|row|candidate_ids.contains(&row.memory.id)).map(|row|{
                let lexical_score=lexical.get(&row.memory.id).map(|rank|2.0/(1.0+*rank as f64)).unwrap_or(0.0);
                let semantic_score=semantic.get(&row.memory.id).copied().unwrap_or(0.0).max(0.0) as f64;
                let scope_score=if row.memory.scope==scope{0.5}else{0.0};
                let days=chrono::DateTime::parse_from_rfc3339(&row.updated_at).map(|at|(now_at-at.with_timezone(&Utc)).num_days().max(0) as f64).unwrap_or(365.0);
                let recency_score=0.25/(1.0+days/30.0);
                let useful_ratio=row.useful_count.max(0) as f64/(1+row.recall_count.max(0)) as f64;
                (lexical_score+semantic_score+scope_score+recency_score+0.5*useful_ratio,row)
            }).collect::<Vec<_>>();
            ranked.sort_by(|a,b|b.0.total_cmp(&a.0).then_with(||b.1.updated_at.cmp(&a.1.updated_at)).then_with(||a.1.memory.id.cmp(&b.1.memory.id)));
            let mut out=Vec::new();let mut bytes=0;
            for (_,row) in ranked.into_iter().take(20){
                let size=serde_json::to_vec(&row.memory)?.len();if bytes+size>6000{continue;}bytes+=size;
                tx.execute("UPDATE memory_embeddings SET recall_count=recall_count+1 WHERE memory_id=?1",[&row.memory.id])?;
                out.push(row.memory);
            }
            tx.commit()?;Ok(out)
        }).await
    }
}
