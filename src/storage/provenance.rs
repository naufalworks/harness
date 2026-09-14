//! P12-T02 seam 1: the provenance edge writer/reader and the bounded incident
//! projection that reads it. Moved verbatim out of `src/storage.rs`; the child
//! module still reaches `DbStore::run` and the private connection pool, so no
//! visibility was widened to make the split possible.

use super::{now, uid, DbStore};
use crate::safety;
use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};

// Exercised by this module's tests. Production edge inserts go through
// agent_loop::steps raw SQL, where migration 006 CHECK constraints enforce the same
// kinds, relations, id lengths and self-edge ban at the database layer.
#[allow(dead_code)]
pub const PROVENANCE_NODE_KINDS: [&str; 6] = [
    "evidence",
    "step",
    "permission",
    "mutation",
    "memory",
    "recovery",
];
#[allow(dead_code)]
pub const PROVENANCE_RELATIONS: [&str; 7] = [
    "supports",
    "contradicts",
    "depends_on",
    "authorizes",
    "mutates",
    "invalidates",
    "triggers",
];

#[allow(dead_code)]
fn validate_provenance_edge(
    source_kind: &str,
    source_id: &str,
    relation: &str,
    target_kind: &str,
    target_id: &str,
) -> Result<()> {
    if !PROVENANCE_NODE_KINDS.contains(&source_kind)
        || !PROVENANCE_NODE_KINDS.contains(&target_kind)
    {
        bail!("unsupported provenance node kind");
    }
    if !PROVENANCE_RELATIONS.contains(&relation) {
        bail!("unsupported provenance relation");
    }
    if source_id.is_empty()
        || source_id.len() > 128
        || target_id.is_empty()
        || target_id.len() > 128
    {
        bail!("provenance row ids must contain 1..128 bytes");
    }
    if source_kind == target_kind && source_id == target_id {
        bail!("a provenance edge cannot point to itself");
    }
    Ok(())
}

impl DbStore {
    /// Persist one inspectable dependency between durable rows. There is deliberately no freeform
    /// reasoning payload: provenance says which rows relate and how, not what the model thought.
    #[allow(dead_code)]
    pub async fn record_provenance_edge(
        &self,
        request_id: String,
        source_kind: String,
        source_id: String,
        relation: String,
        target_kind: String,
        target_id: String,
    ) -> Result<String> {
        validate_provenance_edge(
            &source_kind,
            &source_id,
            &relation,
            &target_kind,
            &target_id,
        )?;
        self.run(move |c| {
            let id = uid();
            c.execute(
                crate::agentic_sql::PROVENANCE_EDGE_INSERT,
                params![
                    id,
                    request_id,
                    source_kind,
                    source_id,
                    relation,
                    target_kind,
                    target_id,
                    now()
                ],
            )?;
            Ok(id)
        })
        .await
    }

    pub async fn provenance_edges(&self, request_id: String) -> Result<Value> {
        self.run(move |c| {
            let mut stmt = c.prepare(crate::agentic_sql::PROVENANCE_EDGES_LIST)?;
            let rows = stmt
                .query_map([request_id], |r| {
                    Ok(json!({
                        "id":r.get::<_,String>(0)?,"source_kind":r.get::<_,String>(1)?,
                        "source_id":r.get::<_,String>(2)?,"relation":r.get::<_,String>(3)?,
                        "target_kind":r.get::<_,String>(4)?,"target_id":r.get::<_,String>(5)?,
                        "created_at":r.get::<_,String>(6)?,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({"edges":rows}))
        })
        .await
    }

    /// Read-only incident projection. It joins bounded, externally inspectable rows and marks
    /// rows without a provenance edge as unknown instead of inventing causal support.
    pub async fn incident_graph(&self, request_id: String) -> Result<Option<Value>> {
        self.run(move |c| {
            let receipt: Option<(String, String, String, String, Option<String>, String)> = c
                .query_row(
                    "SELECT request_id,session_id,scope,state,error_code,updated_at FROM chat_receipts WHERE request_id=?1",
                    [&request_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
                .optional()?;
            let Some((request, session, scope, state, receipt_error, updated_at)) = receipt else {
                return Ok(None);
            };
            let mut nodes = Vec::<Value>::new();
            let mut node_ids = HashSet::<String>::new();
            let mut linked = HashSet::<String>::new();
            let mut upstream = HashMap::<String, Vec<String>>::new();
            let mut downstream = HashMap::<String, Vec<String>>::new();
            let mut breaks = Vec::<(i64, String, String, String)>::new();
            let node_id = |kind: &str, id: &str| format!("{kind}:{id}");
            let mut add = |kind: &str, id: &str, label: String, status: String, known: bool| {
                let full = node_id(kind, id);
                if node_ids.insert(full.clone()) {
                    nodes.push(json!({"id":full,"kind":kind,"row_id":id,"label":label,
                        "status":status,"provenance":if known {"known"} else {"unknown"}}));
                }
                full
            };
            add("request", &request, "coding turn".into(), state.clone(), true);
            if let Some(error) = receipt_error {
                // The receipt is the terminal summary. Prefer an earlier concrete failed or
                // interrupted step when one exists, rather than calling the summary the cause.
                breaks.push((30_000, "request".into(), request.clone(), error));
            }

            let mut stmt = c.prepare("SELECT id,seq,kind,status,COALESCE(tool_name,''),COALESCE(error_code,'') FROM turn_steps WHERE request_id=?1 ORDER BY seq LIMIT 200")?;
            for row in stmt.query_map([&request_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)))? {
                let (id, seq, kind, status, tool, error) = row?;
                let label = if tool.is_empty() { kind.clone() } else { format!("{kind}: {tool}") };
                add("step", &id, safety::redact(&label).chars().take(160).collect(), status.clone(), true);
                if matches!(status.as_str(), "failed" | "denied" | "interrupted") || !error.is_empty() {
                    breaks.push((seq + 1, "step".into(), id, if error.is_empty() { status } else { error }));
                }
            }
            let mut stmt = c.prepare("SELECT id,status,tool_name,summary FROM permission_requests WHERE request_id=?1 ORDER BY created_at LIMIT 200")?;
            for row in stmt.query_map([&request_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))? {
                let (id, status, tool, summary) = row?;
                add("permission", &id, safety::redact(&format!("{tool}: {summary}")).chars().take(240).collect(), status.clone(), true);
                if matches!(status.as_str(), "denied" | "expired") { breaks.push((10_000, "permission".into(), id, status)); }
            }
            let mut stmt = c.prepare("SELECT id,action,path,applied FROM file_changes WHERE request_id=?1 ORDER BY created_at LIMIT 200")?;
            for row in stmt.query_map([&request_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?)))? {
                let (id, action, path, applied) = row?;
                add("mutation", &id, safety::redact(&format!("{action} {path}")).chars().take(240).collect(), if applied == 1 { "applied" } else { "planned" }.into(), true);
            }
            let mut stmt = c.prepare("SELECT seq,kind FROM activity_events WHERE request_id=?1 ORDER BY seq LIMIT 200")?;
            for row in stmt.query_map([&request_id], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))? {
                let (seq, kind) = row?;
                if kind == "interrupted" {
                    let id = seq.to_string();
                    add("recovery", &id, "process interruption recovery".into(), "interrupted".into(), true);
                    breaks.push((20_000, "recovery".into(), id, "interrupted".into()));
                }
            }
            let mut edge_stmt = c.prepare(crate::agentic_sql::PROVENANCE_EDGES_LIST)?;
            let mut edges = Vec::new();
            for row in edge_stmt.query_map([&request_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)))? {
                let (id, source_kind, source_id, relation, target_kind, target_id) = row?;
                let source = node_id(&source_kind, &source_id);
                let target = node_id(&target_kind, &target_id);
                add(&source_kind, &source_id, format!("{source_kind} {source_id}"), "known".into(), true);
                add(&target_kind, &target_id, format!("{target_kind} {target_id}"), "known".into(), true);
                edges.push(json!({"id":id,"source":source,"target":target,"relation":relation}));
            }
            breaks.sort_by_key(|item| item.0);
            let earliest_id = breaks.first().map(|(_, kind, id, _)| node_id(kind, id));
            let mut neighborhood = HashMap::<String, Vec<String>>::new();
            for edge in &edges {
                let Some(source) = edge["source"].as_str() else { continue };
                let Some(target) = edge["target"].as_str() else { continue };
                neighborhood.entry(source.to_string()).or_default().push(target.to_string());
                neighborhood.entry(target.to_string()).or_default().push(source.to_string());
            }
            let seed = earliest_id.as_ref().filter(|id| node_ids.contains(*id)).cloned()
                .unwrap_or_else(|| node_id("request", &request));
            let mut selected = HashSet::<String>::new();
            let mut selected_order = Vec::<String>::new();
            let mut queue = VecDeque::from([seed]);
            while let Some(id) = queue.pop_front() {
                if selected_order.len() == 400 || !selected.insert(id.clone()) { continue; }
                selected_order.push(id.clone());
                if let Some(neighbors) = neighborhood.get(&id) { queue.extend(neighbors.iter().cloned()); }
            }
            for node in &nodes {
                if selected_order.len() == 400 { break; }
                let Some(id) = node["id"].as_str() else { continue };
                if selected.insert(id.to_string()) { selected_order.push(id.to_string()); }
            }
            let total_nodes = nodes.len();
            let total_edges = edges.len();
            let mut nodes_by_id = nodes.drain(..).filter_map(|node| {
                let id = node["id"].as_str()?.to_string();
                Some((id, node))
            }).collect::<HashMap<_, _>>();
            nodes = selected_order.iter().filter_map(|id| nodes_by_id.remove(id)).collect();
            let visible = nodes.iter().filter_map(|node| node["id"].as_str().map(str::to_string)).collect::<HashSet<_>>();
            edges.retain(|edge| {
                edge["source"].as_str().is_some_and(|id| visible.contains(id))
                    && edge["target"].as_str().is_some_and(|id| visible.contains(id))
            });
            edges.truncate(2000);
            for edge in &edges {
                let Some(source) = edge["source"].as_str() else { continue };
                let Some(target) = edge["target"].as_str() else { continue };
                linked.insert(source.to_string());
                linked.insert(target.to_string());
                downstream.entry(source.to_string()).or_default().push(target.to_string());
                upstream.entry(target.to_string()).or_default().push(source.to_string());
            }
            let mut unknown = Vec::new();
            for node in &mut nodes {
                let id = node["id"].as_str().unwrap_or_default().to_string();
                node["upstream"] = json!(upstream.get(&id).cloned().unwrap_or_default());
                node["downstream"] = json!(downstream.get(&id).cloned().unwrap_or_default());
                if node["kind"] != json!("request") && !linked.contains(&id) {
                    node["provenance"] = json!("unknown");
                    unknown.push(json!({"node_id":id,"reason":"no recorded provenance edge"}));
                }
            }
            let earliest = breaks.first().map(|(_, kind, id, reason)| json!({"node_id":node_id(kind,id),"kind":kind,"row_id":id,"reason":safety::redact(reason),"known":true})).unwrap_or_else(|| json!({"node_id":Value::Null,"kind":"unknown","row_id":Value::Null,"reason":"no recorded failure or recovery break was found","known":false}));
            unknown.truncate(400);
            let returned_nodes = nodes.len();
            let returned_edges = edges.len();
            let omitted_nodes = total_nodes.saturating_sub(returned_nodes);
            let omitted_edges = total_edges.saturating_sub(returned_edges);
            let node_cursor = (omitted_nodes > 0).then(|| json!({"projection":"causal-neighborhood-v1","break_node_id":earliest_id,"after_node_id":nodes.last().and_then(|node| node["id"].as_str())}));
            let edge_cursor = (omitted_edges > 0).then(|| json!({"projection":"causal-neighborhood-v1","break_node_id":earliest_id,"after_edge_id":edges.last().and_then(|edge| edge["id"].as_str())}));
            let truncation = json!({"nodes":omitted_nodes > 0,"edges":omitted_edges > 0});
            Ok(Some(json!({"request":{"request_id":request,"session_id":session,"scope":scope,"state":state,"updated_at":updated_at},"nodes":nodes,"edges":edges,"unknown_provenance":unknown,"earliest_known_break":earliest,"bounds":{"max_nodes":400,"max_edges":2000},"truncated":truncation,"counts":{"nodes":{"total":total_nodes,"returned":returned_nodes,"omitted":omitted_nodes},"edges":{"total":total_edges,"returned":returned_edges,"omitted":omitted_edges}},"expansion_cursors":{"nodes":node_cursor,"edges":edge_cursor}})))
        }).await
    }
}
