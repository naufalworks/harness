//! P12-T02 seam 1: the provenance edge writer/reader and the bounded incident
//! projection that reads it. Moved verbatim out of `src/storage.rs`; the child
//! module still reaches `DbStore::run` and the private connection pool, so no
//! visibility was widened to make the split possible.
//!
//! P16-T01 extends that projection with reviewer navigation — bounded expansion,
//! search/filter by path/tool/relation/status/row, and a chronological view beside the
//! causal one — without adding a second projection. `incident_graph` is now a thin
//! call into [`DbStore::incident_view`] with default query parameters, so the read model
//! a reviewer filters is byte-for-byte the read model the unfiltered endpoint returns.
//!
//! Confidence labelling is deliberate and conservative. An edge that was recorded is
//! `recorded_dependency`. A row that merely sits in the same request with a recorded
//! timestamp and no edge is `temporal_proximity` — co-occurrence, never causation. A row
//! with neither is `unknown`. Nothing here infers a dependency from ordering, and no
//! field carries model reasoning: labels are computed from rows that already exist.

use super::{now, uid, DbStore};
use crate::export::review;
use crate::safety;
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
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

/// The projection identity clients see in an expansion cursor. Bumping this invalidates
/// old cursors on purpose: a cursor is only meaningful against the ordering that made it.
pub const PROJECTION: &str = "causal-neighborhood-v1";

/// Bounds shared by every incident read. A reviewer may narrow with filters but never widen.
const MAX_NODES: usize = 400;
const MAX_EDGES: usize = 2000;
const MAX_TIMELINE: usize = 400;
/// Per-source-table read bound, unchanged from the pre-P16-T01 projection.
const ROW_LIMIT: i64 = 200;

/// The three confidence labels, and what each one is allowed to mean. Exposed in the
/// response as a legend so a client cannot invent a fourth meaning for a label it sees.
fn confidence_legend() -> Value {
    json!({
        "recorded_dependency": "A provenance edge was recorded between these rows.",
        "temporal_proximity": "The row occurred inside the same request and carries a recorded time, but no dependency was recorded. Co-occurrence is not causation.",
        "unknown": "No dependency and no usable recorded time. Nothing is claimed."
    })
}

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

/// What a reviewer asked for. Every field is optional: the default value of this struct
/// reproduces the pre-P16-T01 response exactly, which is why there is only one projection.
#[derive(Clone, Debug)]
pub struct IncidentQuery {
    /// `causal` (default) or `chronological`. Both views are computed from the same
    /// selected node set; the view only decides which ordering is authoritative.
    pub view: String,
    pub relation: Option<String>,
    pub kind: Option<String>,
    pub status: Option<String>,
    /// Substring match against a mutation's path.
    pub path: Option<String>,
    /// Substring match against a step's or permission's tool name.
    pub tool: Option<String>,
    /// Exact match against the durable row id behind a node.
    pub row_id: Option<String>,
    /// Free-text terms matched against the node label. Tokenised by the same
    /// `safety::fts_query` normaliser history search uses, so a reviewer's query means
    /// the same thing in both places.
    pub q: Option<String>,
    /// An opaque cursor from a previous response's `expansion_cursors`, passed back
    /// unchanged. Constructing one by hand is refused.
    pub anchor: Option<Value>,
}

impl Default for IncidentQuery {
    fn default() -> Self {
        Self {
            view: "causal".into(),
            relation: None,
            kind: None,
            status: None,
            path: None,
            tool: None,
            row_id: None,
            q: None,
            anchor: None,
        }
    }
}

impl IncidentQuery {
    pub fn validate(&self) -> Result<()> {
        if self.view != "causal" && self.view != "chronological" {
            bail!("view must be causal or chronological");
        }
        if let Some(relation) = self.relation.as_deref() {
            if !PROVENANCE_RELATIONS.contains(&relation) {
                bail!("unsupported provenance relation filter");
            }
        }
        for (name, value) in [
            ("kind", &self.kind),
            ("status", &self.status),
            ("path", &self.path),
            ("tool", &self.tool),
            ("row_id", &self.row_id),
            ("q", &self.q),
        ] {
            if let Some(text) = value {
                if text.len() > 200 {
                    bail!("{name} filter must be at most 200 bytes");
                }
            }
        }
        if let Some(anchor) = &self.anchor {
            if anchor["projection"] != json!(PROJECTION) {
                bail!("expansion anchor was not produced by this projection");
            }
        }
        Ok(())
    }

    fn is_filtered(&self) -> bool {
        self.relation.is_some()
            || self.kind.is_some()
            || self.status.is_some()
            || self.path.is_some()
            || self.tool.is_some()
            || self.row_id.is_some()
            || self.q.is_some()
    }

    fn as_json(&self) -> Value {
        json!({
            "view": self.view,
            "relation": self.relation,
            "kind": self.kind,
            "status": self.status,
            "path": self.path,
            "tool": self.tool,
            "row_id": self.row_id,
            "q": self.q,
            "filtered": self.is_filtered(),
        })
    }
}

/// One node as gathered from durable rows, before any filtering or bounding. `path`,
/// `tool`, `at` and `seq` are gathered here so filtering and the chronological view read
/// the same values the causal view labels with.
struct RawNode {
    id: String,
    kind: String,
    row_id: String,
    label: String,
    status: String,
    path: Option<String>,
    tool: Option<String>,
    at: Option<String>,
    seq: i64,
}

impl RawNode {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind,
            "row_id": self.row_id,
            "label": self.label,
            "status": self.status,
            "path": self.path,
            "tool": self.tool,
            "at": self.at,
            "seq": self.seq,
        })
    }
}

struct Incident {
    request: Value,
    nodes: Vec<RawNode>,
    edges: Vec<Value>,
    /// (rank, kind, row id, reason) for every recorded break, lowest rank first.
    breaks: Vec<(i64, String, String, String)>,
}

/// Gather the durable rows behind one request. This is the only place that reads them:
/// both the causal and the chronological view, and every filter, work off this result.
fn collect(c: &Connection, request_id: &str) -> Result<Option<Incident>> {
    let receipt: Option<(String, String, String, String, Option<String>, String)> = c
        .query_row(
            "SELECT request_id,session_id,scope,state,error_code,updated_at FROM chat_receipts WHERE request_id=?1",
            [request_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()?;
    let Some((request, session, scope, state, receipt_error, updated_at)) = receipt else {
        return Ok(None);
    };
    let mut nodes = Vec::<RawNode>::new();
    let mut seen = HashSet::<String>::new();
    let mut breaks = Vec::<(i64, String, String, String)>::new();
    let mut push = |node: RawNode| {
        if seen.insert(node.id.clone()) {
            nodes.push(node);
        }
    };
    push(RawNode {
        id: format!("request:{request}"),
        kind: "request".into(),
        row_id: request.clone(),
        label: "coding turn".into(),
        status: state.clone(),
        path: None,
        tool: None,
        at: Some(updated_at.clone()),
        seq: -1,
    });
    if let Some(error) = receipt_error {
        // The receipt is the terminal summary. Prefer an earlier concrete failed or
        // interrupted step when one exists, rather than calling the summary the cause.
        breaks.push((30_000, "request".into(), request.clone(), error));
    }

    let mut stmt = c.prepare("SELECT id,seq,kind,status,COALESCE(tool_name,''),COALESCE(error_code,''),started_at FROM turn_steps WHERE request_id=?1 ORDER BY seq LIMIT ?2")?;
    for row in stmt.query_map(params![request_id, ROW_LIMIT], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    })? {
        let (id, seq, kind, status, tool, error, at) = row?;
        let text = if tool.is_empty() {
            kind.clone()
        } else {
            format!("{kind}: {tool}")
        };
        push(RawNode {
            id: format!("step:{id}"),
            kind: "step".into(),
            row_id: id.clone(),
            label: label(&text),
            status: status.clone(),
            path: None,
            tool: (!tool.is_empty()).then(|| tool.clone()),
            at,
            seq,
        });
        if matches!(status.as_str(), "failed" | "denied" | "interrupted") || !error.is_empty() {
            breaks.push((
                seq + 1,
                "step".into(),
                id,
                if error.is_empty() { status } else { error },
            ));
        }
    }
    let mut stmt = c.prepare("SELECT id,status,tool_name,summary,created_at FROM permission_requests WHERE request_id=?1 ORDER BY created_at LIMIT ?2")?;
    for row in stmt.query_map(params![request_id, ROW_LIMIT], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })? {
        let (id, status, tool, summary, at) = row?;
        push(RawNode {
            id: format!("permission:{id}"),
            kind: "permission".into(),
            row_id: id.clone(),
            label: label(&format!("{tool}: {summary}")),
            status: status.clone(),
            path: None,
            tool: Some(tool),
            at,
            seq: 10_000,
        });
        if matches!(status.as_str(), "denied" | "expired") {
            breaks.push((10_000, "permission".into(), id, status));
        }
    }
    let mut stmt = c.prepare("SELECT id,action,path,applied,created_at FROM file_changes WHERE request_id=?1 ORDER BY created_at LIMIT ?2")?;
    for row in stmt.query_map(params![request_id, ROW_LIMIT], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })? {
        let (id, action, path, applied, at) = row?;
        push(RawNode {
            id: format!("mutation:{id}"),
            kind: "mutation".into(),
            row_id: id,
            label: label(&format!("{action} {path}")),
            status: if applied == 1 { "applied" } else { "planned" }.into(),
            path: Some(path),
            tool: None,
            at,
            seq: 15_000,
        });
    }
    let mut stmt = c.prepare(
        "SELECT seq,kind,created_at FROM activity_events WHERE request_id=?1 ORDER BY seq LIMIT ?2",
    )?;
    for row in stmt.query_map(params![request_id, ROW_LIMIT], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })? {
        let (seq, kind, at) = row?;
        if kind == "interrupted" {
            let id = seq.to_string();
            push(RawNode {
                id: format!("recovery:{id}"),
                kind: "recovery".into(),
                row_id: id.clone(),
                label: "process interruption recovery".into(),
                status: "interrupted".into(),
                path: None,
                tool: None,
                at,
                seq: 20_000,
            });
            breaks.push((20_000, "recovery".into(), id, "interrupted".into()));
        }
    }

    let mut edge_stmt = c.prepare(crate::agentic_sql::PROVENANCE_EDGES_LIST)?;
    let mut edges = Vec::new();
    for row in edge_stmt.query_map([request_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
        ))
    })? {
        let (id, source_kind, source_id, relation, target_kind, target_id) = row?;
        let source = format!("{source_kind}:{source_id}");
        let target = format!("{target_kind}:{target_id}");
        // An edge endpoint that no source-table read produced still has to resolve to a
        // node, or the response would expose an identifier with nothing behind it.
        for (full, kind, row_id) in [
            (&source, &source_kind, &source_id),
            (&target, &target_kind, &target_id),
        ] {
            push(RawNode {
                id: full.clone(),
                kind: kind.clone(),
                row_id: row_id.clone(),
                label: format!("{kind} {row_id}"),
                status: "known".into(),
                path: None,
                tool: None,
                at: None,
                seq: 25_000,
            });
        }
        edges.push(json!({
            "id": id,
            "source": source,
            "target": target,
            "relation": relation,
            "confidence": "recorded_dependency",
        }));
    }
    breaks.sort_by_key(|item| item.0);
    Ok(Some(Incident {
        request: json!({
            "request_id": request,
            "session_id": session,
            "scope": scope,
            "state": state,
            "updated_at": updated_at,
        }),
        nodes,
        edges,
        breaks,
    }))
}

/// Redact and bound a node label. Labels come from recorded tool names, paths and
/// summaries, so they are redacted before they are ever returned.
fn label(text: &str) -> String {
    safety::redact(text).chars().take(240).collect()
}

/// Tokenise a reviewer's free-text query the same way history search does, then reduce it
/// to plain terms for substring matching against labels. Reusing `safety::fts_query`
/// means an incident query and a history query treat punctuation identically instead of
/// two hand-rolled tokenisers drifting apart.
fn terms(query: &str) -> Vec<String> {
    safety::fts_query(query)
        .split_whitespace()
        .map(|term| term.trim_matches(|c: char| !c.is_alphanumeric() && c != '_'))
        .filter(|term| !term.is_empty())
        .map(|term| term.to_lowercase())
        .collect()
}

fn matches(node: &RawNode, query: &IncidentQuery, query_terms: &[String]) -> bool {
    if let Some(kind) = query.kind.as_deref() {
        if node.kind != kind {
            return false;
        }
    }
    if let Some(status) = query.status.as_deref() {
        if node.status != status {
            return false;
        }
    }
    if let Some(row_id) = query.row_id.as_deref() {
        if node.row_id != row_id {
            return false;
        }
    }
    if let Some(path) = query.path.as_deref() {
        let needle = path.to_lowercase();
        if !node
            .path
            .as_deref()
            .is_some_and(|value| value.to_lowercase().contains(&needle))
        {
            return false;
        }
    }
    if let Some(tool) = query.tool.as_deref() {
        let needle = tool.to_lowercase();
        if !node
            .tool
            .as_deref()
            .is_some_and(|value| value.to_lowercase().contains(&needle))
        {
            return false;
        }
    }
    if !query_terms.is_empty() {
        let haystack = format!(
            "{} {} {} {}",
            node.label,
            node.kind,
            node.status,
            node.path.as_deref().unwrap_or_default()
        )
        .to_lowercase();
        // Every term must appear: AND matching, the same default `safety::fts_query`
        // produces for history search.
        if !query_terms.iter().all(|term| haystack.contains(term)) {
            return false;
        }
    }
    true
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
    ///
    /// Kept as the unfiltered entry point; the projection itself lives in
    /// [`Self::incident_view`], so there is one read model rather than two.
    ///
    /// Only tests call it since P16-T01 gave the route filters; it is retained because it is
    /// the executable statement that a default query reproduces the pre-P16-T01 response.
    #[allow(dead_code)]
    pub async fn incident_graph(&self, request_id: String) -> Result<Option<Value>> {
        self.incident_view(request_id, IncidentQuery::default())
            .await
    }

    /// P16-T01: the reviewer-facing incident read model — bounded expansion, filtering by
    /// path/tool/relation/status/row/kind/text, a chronological view beside the causal one,
    /// and confidence labels that separate a recorded dependency from mere co-occurrence.
    pub async fn incident_view(
        &self,
        request_id: String,
        query: IncidentQuery,
    ) -> Result<Option<Value>> {
        query.validate()?;
        self.read(move |c| {
            let Some(incident) = collect(c, &request_id)? else {
                return Ok(None);
            };
            let Incident {
                request,
                nodes,
                mut edges,
                breaks,
            } = incident;
            let total_nodes_unfiltered = nodes.len();
            let total_edges_unfiltered = edges.len();

            // Relation filtering happens on edges before node selection, so filtering by
            // `depends_on` narrows the neighborhood rather than showing the same nodes with
            // fewer lines drawn between them.
            if let Some(relation) = query.relation.as_deref() {
                edges.retain(|edge| edge["relation"] == json!(relation));
            }

            let query_terms = query.q.as_deref().map(terms).unwrap_or_default();
            let request_node_id = request["request_id"]
                .as_str()
                .map(|id| format!("request:{id}"))
                .unwrap_or_default();
            // The request node always survives filtering: it is the frame the response is
            // about, and dropping it would leave a graph with no anchor to expand from.
            let matched: Vec<&RawNode> = nodes
                .iter()
                .filter(|node| node.id == request_node_id || matches(node, &query, &query_terms))
                .collect();
            let matched_ids: HashSet<&str> = matched.iter().map(|node| node.id.as_str()).collect();
            edges.retain(|edge| {
                edge["source"]
                    .as_str()
                    .is_some_and(|id| matched_ids.contains(id))
                    && edge["target"]
                        .as_str()
                        .is_some_and(|id| matched_ids.contains(id))
            });

            // Adjacency over the surviving edges only.
            let mut neighborhood = HashMap::<&str, Vec<&str>>::new();
            for edge in &edges {
                let Some(source) = edge["source"].as_str() else {
                    continue;
                };
                let Some(target) = edge["target"].as_str() else {
                    continue;
                };
                neighborhood.entry(source).or_default().push(target);
                neighborhood.entry(target).or_default().push(source);
            }

            let earliest_id = breaks
                .first()
                .map(|(_, kind, id, _)| format!("{kind}:{id}"));
            let seed = earliest_id
                .as_deref()
                .filter(|id| matched_ids.contains(id))
                .map(str::to_string)
                .unwrap_or_else(|| request_node_id.clone());

            // Expansion. An anchor names the last node the previous page returned; the fill
            // phase resumes after it in durable order. The break neighborhood is still
            // seeded first, because a later page that lost the break would describe a
            // different incident than the one the reviewer is reading.
            let after = query
                .anchor
                .as_ref()
                .and_then(|anchor| anchor["after_node_id"].as_str())
                .map(str::to_string);

            let mut selected = HashSet::<String>::new();
            let mut order = Vec::<String>::new();
            let mut queue = VecDeque::from([seed]);
            while let Some(id) = queue.pop_front() {
                if order.len() == MAX_NODES || !matched_ids.contains(id.as_str()) {
                    continue;
                }
                if !selected.insert(id.clone()) {
                    continue;
                }
                order.push(id.clone());
                if let Some(neighbors) = neighborhood.get(id.as_str()) {
                    queue.extend(neighbors.iter().map(|id| id.to_string()));
                }
            }
            let mut resuming = after.is_some();
            for node in &matched {
                if order.len() == MAX_NODES {
                    break;
                }
                if resuming {
                    if Some(node.id.as_str()) == after.as_deref() {
                        resuming = false;
                    }
                    // Already-selected neighborhood nodes are never skipped by the cursor:
                    // the closed-graph guarantee outranks pagination tidiness.
                    if !selected.contains(&node.id) {
                        continue;
                    }
                }
                if selected.insert(node.id.clone()) {
                    order.push(node.id.clone());
                }
            }

            let total_nodes = matched.len();
            let total_edges = edges.len();
            let by_id: HashMap<&str, &RawNode> =
                matched.iter().map(|node| (node.id.as_str(), *node)).collect();
            let selected_nodes: Vec<&RawNode> = order
                .iter()
                .filter_map(|id| by_id.get(id.as_str()).copied())
                .collect();
            let visible: HashSet<&str> = selected_nodes.iter().map(|n| n.id.as_str()).collect();
            edges.retain(|edge| {
                edge["source"].as_str().is_some_and(|id| visible.contains(id))
                    && edge["target"].as_str().is_some_and(|id| visible.contains(id))
            });
            edges.truncate(MAX_EDGES);

            let mut upstream = HashMap::<String, Vec<String>>::new();
            let mut downstream = HashMap::<String, Vec<String>>::new();
            let mut linked = HashSet::<String>::new();
            for edge in &edges {
                let Some(source) = edge["source"].as_str() else {
                    continue;
                };
                let Some(target) = edge["target"].as_str() else {
                    continue;
                };
                linked.insert(source.to_string());
                linked.insert(target.to_string());
                downstream
                    .entry(source.to_string())
                    .or_default()
                    .push(target.to_string());
                upstream
                    .entry(target.to_string())
                    .or_default()
                    .push(source.to_string());
            }

            let mut unknown = Vec::new();
            let mut confidence_counts: HashMap<&str, i64> = HashMap::new();
            let mut out_nodes = Vec::<Value>::new();
            for raw in &selected_nodes {
                let mut node = raw.to_json();
                let id = raw.id.clone();
                node["upstream"] = json!(upstream.get(&id).cloned().unwrap_or_default());
                node["downstream"] = json!(downstream.get(&id).cloned().unwrap_or_default());
                let is_request = raw.kind == "request";
                let known = is_request || linked.contains(&id);
                // Three labels, in decreasing strength. Nothing here upgrades proximity to
                // dependency: the only way to earn `recorded_dependency` is a recorded edge.
                let confidence = if known {
                    "recorded_dependency"
                } else if raw.at.as_deref().is_some_and(|at| !at.is_empty()) {
                    "temporal_proximity"
                } else {
                    "unknown"
                };
                node["confidence"] = json!(confidence);
                node["confidence_basis"] = json!(match confidence {
                    "recorded_dependency" => "provenance_edge",
                    "temporal_proximity" => "same_request_recorded_time",
                    _ => "no_evidence",
                });
                node["provenance"] = json!(if known { "known" } else { "unknown" });
                *confidence_counts.entry(confidence).or_default() += 1;
                if !known {
                    unknown.push(json!({
                        "node_id": id,
                        "reason": "no recorded provenance edge",
                        "confidence": confidence,
                    }));
                }
                out_nodes.push(node);
            }
            unknown.truncate(MAX_NODES);

            // Chronological view. Rows with no recorded time are not guessed into an
            // ordering: they are listed last and counted as undated.
            let mut timeline: Vec<Value> = out_nodes
                .iter()
                .map(|node| {
                    json!({
                        "node_id": node["id"],
                        "kind": node["kind"],
                        "row_id": node["row_id"],
                        "label": node["label"],
                        "status": node["status"],
                        "at": node["at"],
                        "seq": node["seq"],
                        "confidence": node["confidence"],
                    })
                })
                .collect();
            timeline.sort_by(|a, b| {
                let key = |item: &Value| {
                    (
                        item["at"].as_str().is_none(),
                        item["at"].as_str().unwrap_or_default().to_string(),
                        item["seq"].as_i64().unwrap_or_default(),
                    )
                };
                key(a).cmp(&key(b))
            });
            let undated = timeline
                .iter()
                .filter(|item| item["at"].as_str().is_none())
                .count();
            timeline.truncate(MAX_TIMELINE);

            let earliest = breaks
                .first()
                .filter(|(_, kind, id, _)| visible.contains(format!("{kind}:{id}").as_str()))
                .map(|(_, kind, id, reason)| {
                    json!({
                        "node_id": format!("{kind}:{id}"),
                        "kind": kind,
                        "row_id": id,
                        "reason": safety::redact(reason),
                        "known": true,
                        "confidence": "recorded_dependency",
                    })
                })
                .unwrap_or_else(|| {
                    json!({
                        "node_id": Value::Null,
                        "kind": "unknown",
                        "row_id": Value::Null,
                        "reason": "no recorded failure or recovery break was found",
                        "known": false,
                        "confidence": "unknown",
                    })
                });

            let returned_nodes = out_nodes.len();
            let returned_edges = edges.len();
            let omitted_nodes = total_nodes.saturating_sub(returned_nodes);
            let omitted_edges = total_edges.saturating_sub(returned_edges);
            let break_node_id = earliest_id.clone();
            let node_cursor = (omitted_nodes > 0).then(|| {
                json!({
                    "projection": PROJECTION,
                    "break_node_id": break_node_id,
                    "after_node_id": out_nodes.last().and_then(|node| node["id"].as_str()),
                })
            });
            let edge_cursor = (omitted_edges > 0).then(|| {
                json!({
                    "projection": PROJECTION,
                    "break_node_id": earliest_id,
                    "after_edge_id": edges.last().and_then(|edge| edge["id"].as_str()),
                })
            });

            Ok(Some(json!({
                "request": request,
                "projection": PROJECTION,
                "query": query.as_json(),
                "nodes": out_nodes,
                "edges": edges,
                "timeline": timeline,
                "timeline_undated": undated,
                "unknown_provenance": unknown,
                "earliest_known_break": earliest,
                "bounds": {"max_nodes": MAX_NODES, "max_edges": MAX_EDGES, "max_timeline": MAX_TIMELINE},
                "truncated": {"nodes": omitted_nodes > 0, "edges": omitted_edges > 0},
                "counts": {
                    "nodes": {"total": total_nodes, "returned": returned_nodes, "omitted": omitted_nodes},
                    "edges": {"total": total_edges, "returned": returned_edges, "omitted": omitted_edges},
                    "unfiltered": {"nodes": total_nodes_unfiltered, "edges": total_edges_unfiltered},
                    "confidence": {
                        "recorded_dependency": confidence_counts.get("recorded_dependency").copied().unwrap_or_default(),
                        "temporal_proximity": confidence_counts.get("temporal_proximity").copied().unwrap_or_default(),
                        "unknown": confidence_counts.get("unknown").copied().unwrap_or_default(),
                    },
                },
                "confidence_labels": confidence_legend(),
                "sanitizer": review::SANITIZER,
                "expansion_cursors": {"nodes": node_cursor, "edges": edge_cursor},
                "note": "Diagnostic provenance over recorded rows. A label of temporal_proximity means co-occurrence only, never causation, and no field carries model reasoning."
            })))
        })
        .await
    }
}
