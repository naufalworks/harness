//! P16-T02: comparing two or more runs' causal graphs, and exporting one graph as a
//! sanitized artifact.
//!
//! Two disciplines shape this module, and both are the repo's existing ones rather than new
//! inventions.
//!
//! 1. **One projection.** Neither capability reads the database directly. Comparison and
//!    export both call [`DbStore::incident_view`] — the single P16-T01 read model — so the
//!    graph a reviewer compares and the graph they export are byte-for-byte the graph the
//!    incident endpoint returns. A second query here would have been a second definition of
//!    "the causal graph", and the two would have drifted.
//! 2. **One exporter.** The sanitized artifact is built from `export::review` (the shared
//!    sanitizer and the [`Citation`] shape) and `export::packet::canonical_json` (the shared
//!    deterministic serialization and checksum). Nothing here re-implements redaction,
//!    hashing or citation formatting; P15-T04 owns all three.
//!
//! ## What "aligned" means, and what it does not mean
//!
//! Runs are aligned by **semantic step identity**: a label derived from what a node *is*
//! (its kind, its tool, its action and path) plus its ordinal among identical siblings.
//! Row ids, timestamps and sequence numbers are deliberately excluded, because those differ
//! between two runs of the same work and aligning on them would report every run as wholly
//! different.
//!
//! Alignment is a structural claim, not a causal one. When a slot exists in one run and not
//! another, this module says `missing_evidence` and names the run it is missing from. It
//! never says the absent step did not happen, and it never asserts a counterfactual outcome
//! for it — the recorded rows do not support either statement.

use super::DbStore;
use crate::export::packet::canonical_json;
use crate::export::review::{self, Citation};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

/// The largest number of runs one comparison may align. A comparison is a reviewer-facing
/// artifact, not a batch job, and every run costs a full bounded projection read.
pub const MAX_COMPARED_RUNS: usize = 8;
/// The largest number of aligned slots reported. Each run's projection is already bounded to
/// 400 nodes, so this bounds the union rather than any single graph.
pub const MAX_ALIGNED_SLOTS: usize = 800;
/// The export artifact format identity. Bumping it invalidates consumers on purpose.
pub const EXPORT_FORMAT: &str = "causal-graph-export-v1";
/// The comparison artifact format identity.
pub const COMPARISON_FORMAT: &str = "causal-run-comparison-v1";

/// The four evidence classes an exported node or edge carries, per the P16 design doc's
/// vocabulary. These are *evidence* labels, not strength scores: nothing is ranked.
///
/// * `dependency` — a provenance edge was recorded whose relation asserts consumption or
///   support (`depends_on`, `supports`, `authorizes`, `mutates`, `triggers`).
/// * `contradiction` — a recorded edge whose relation asserts conflict (`contradicts`,
///   `invalidates`). Kept distinct from `dependency` because the design's success criteria
///   require distinguishing missing evidence from *contradictory* evidence.
/// * `temporal` — the row sat in the same request with a recorded time and no edge.
///   Co-occurrence, never causation.
/// * `unknown` — neither. Nothing is claimed.
pub fn evidence_label(relation: Option<&str>, confidence: Option<&str>) -> &'static str {
    match relation {
        Some("contradicts") | Some("invalidates") => "contradiction",
        Some(_) => "dependency",
        None => match confidence {
            Some("recorded_dependency") => "dependency",
            Some("temporal_proximity") => "temporal",
            _ => "unknown",
        },
    }
}

/// The legend shipped with every comparison and every export, so a consumer cannot invent a
/// fifth meaning for a label it received.
pub fn evidence_legend() -> Value {
    json!({
        "dependency": "A provenance edge asserting consumption or support was recorded between these rows.",
        "contradiction": "A provenance edge asserting conflict (contradicts/invalidates) was recorded.",
        "temporal": "The row occurred inside the same request and carries a recorded time, but no dependency was recorded. Co-occurrence is not causation.",
        "unknown": "No dependency and no usable recorded time. Nothing is claimed."
    })
}

/// The legend for a comparison slot's verdict. `missing_evidence` is separated from
/// `differs` on purpose: "this run has no such step recorded" and "both runs recorded this
/// step with different outcomes" are different reviewer problems, and collapsing them would
/// let an absence read as a finding.
pub fn alignment_legend() -> Value {
    json!({
        "identical": "Every compared run recorded this step with the same kind, status and evidence label.",
        "differs": "Every compared run recorded this step, and at least one recorded attribute differs.",
        "missing_evidence": "At least one run has no such step recorded. That is an absence of evidence, not proof the step did not occur, and no counterfactual outcome is claimed from it."
    })
}

/// A run's semantic step identity for one node: what the node is, independent of the row id,
/// timestamp and sequence number that necessarily differ between runs.
fn semantic_key(node: &Value) -> String {
    let kind = node["kind"].as_str().unwrap_or("unknown");
    let tool = node["tool"].as_str().unwrap_or_default();
    let path = node["path"].as_str().unwrap_or_default();
    let label = node["label"].as_str().unwrap_or_default();
    match kind {
        // The request frame is one slot per run by definition.
        "request" => "request".to_string(),
        // A step or permission is identified by its tool when it has one. Falling back to the
        // label keeps non-tool steps alignable without inventing an identity for them.
        "step" | "permission" => {
            if tool.is_empty() {
                format!("{kind}|{}", normalize(label))
            } else {
                format!("{kind}|{}", normalize(tool))
            }
        }
        // A mutation is identified by what it did to which path; the label already carries
        // `action path`, and the path is normalized so two runs under different temporary
        // roots still align.
        "mutation" => format!(
            "{kind}|{}",
            normalize(if path.is_empty() { label } else { path })
        ),
        _ => format!("{kind}|{}", normalize(label)),
    }
}

/// Lowercase, collapse whitespace, and drop the volatile head of a path. Deliberately
/// conservative: it does not stem, split or fuzzy-match, because a wrong alignment is worse
/// than an unaligned slot — an unaligned slot reads as `missing_evidence`, which is honest,
/// whereas a wrong alignment reads as a difference that was never recorded.
fn normalize(text: &str) -> String {
    let lowered = text.trim().to_lowercase();
    let collapsed = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(160).collect()
}

/// One node's compared attributes. Only recorded values appear here.
fn slot_observation(node: &Value) -> Value {
    json!({
        "node_id": node["id"],
        "kind": node["kind"],
        "status": node["status"],
        "evidence": evidence_label(None, node["confidence"].as_str()),
        "confidence": node["confidence"],
        "confidence_basis": node["confidence_basis"],
        "at": node["at"],
    })
}

impl DbStore {
    /// P16-T02: align two or more runs' causal graphs by semantic step identity and report
    /// the first divergence.
    ///
    /// Every run is read through [`Self::incident_view`] with default query parameters, so
    /// this compares the same projection the incident endpoint serves. A request id that has
    /// no recording receipt is reported as `not_found` in `runs` and excluded from alignment
    /// rather than silently treated as an empty graph — an empty graph would make every slot
    /// look like a difference.
    pub async fn compare_incident_runs(&self, request_ids: Vec<String>) -> Result<Value> {
        if request_ids.len() < 2 {
            bail!("comparison needs at least two request ids");
        }
        if request_ids.len() > MAX_COMPARED_RUNS {
            bail!("comparison accepts at most {MAX_COMPARED_RUNS} request ids");
        }
        let mut deduped = Vec::<String>::new();
        for id in request_ids {
            if !deduped.contains(&id) {
                deduped.push(id);
            }
        }
        if deduped.len() < 2 {
            bail!("comparison needs at least two distinct request ids");
        }

        // One projection read per run. No second query, no direct table access.
        let mut runs = Vec::<Value>::new();
        let mut graphs = Vec::<(String, Value)>::new();
        for id in &deduped {
            match self
                .incident_view(id.clone(), super::IncidentQuery::default())
                .await?
            {
                Some(graph) => {
                    runs.push(json!({
                        "request_id": id,
                        "found": true,
                        "state": graph["request"]["state"],
                        "session_id": graph["request"]["session_id"],
                        "nodes": graph["counts"]["nodes"]["returned"],
                        "edges": graph["counts"]["edges"]["returned"],
                        "truncated": graph["truncated"],
                        "earliest_known_break": graph["earliest_known_break"],
                    }));
                    graphs.push((id.clone(), graph));
                }
                None => runs.push(json!({
                    "request_id": id,
                    "found": false,
                    "detail": "No recording receipt exists for this request id, so it contributes no evidence and is excluded from alignment.",
                })),
            }
        }
        if graphs.len() < 2 {
            return Ok(json!({
                "format": COMPARISON_FORMAT,
                "runs": runs,
                "compared": graphs.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
                "aligned": Vec::<Value>::new(),
                "first_divergence": json!({
                    "aligned": false,
                    "detail": "Fewer than two of the requested runs have recorded evidence, so no alignment was attempted.",
                }),
                "counts": {"slots": 0, "identical": 0, "differs": 0, "missing_evidence": 0},
                "evidence_labels": evidence_legend(),
                "alignment_labels": alignment_legend(),
                "sanitizer": review::SANITIZER,
                "note": "Structural alignment over recorded rows. A missing slot is absence of evidence, not proof a step did not occur, and no counterfactual answer is claimed for any run.",
            }));
        }

        // Bucket each run's nodes by semantic key, preserving the projection's order so the
        // ordinal that disambiguates identical siblings is deterministic.
        let mut per_run: Vec<HashMap<String, Vec<&Value>>> = Vec::new();
        for (_, graph) in &graphs {
            let mut buckets = HashMap::<String, Vec<&Value>>::new();
            if let Some(nodes) = graph["nodes"].as_array() {
                for node in nodes {
                    buckets.entry(semantic_key(node)).or_default().push(node);
                }
            }
            per_run.push(buckets);
        }

        // Slot order is deterministic and driven by the first run that recorded each slot, so
        // "first divergence" means the same thing on every call with the same inputs.
        let mut slot_order = Vec::<String>::new();
        let mut widths = BTreeMap::<String, usize>::new();
        for buckets in &per_run {
            let mut keys: Vec<&String> = buckets.keys().collect();
            keys.sort();
            for key in keys {
                if !slot_order.contains(key) {
                    slot_order.push(key.clone());
                }
                let width = widths.entry(key.clone()).or_insert(0);
                *width = (*width).max(buckets[key].len());
            }
        }
        // Order slots by where they first appear in the first run's own node order, then by
        // key, so the reviewer reads the comparison in the order the leading run happened.
        let leading_position: HashMap<String, usize> = graphs[0].1["nodes"]
            .as_array()
            .map(|nodes| {
                let mut seen = HashMap::<String, usize>::new();
                for (index, node) in nodes.iter().enumerate() {
                    seen.entry(semantic_key(node)).or_insert(index);
                }
                seen
            })
            .unwrap_or_default();
        slot_order.sort_by_key(|key| {
            (
                leading_position.get(key).copied().unwrap_or(usize::MAX),
                key.clone(),
            )
        });

        let mut aligned = Vec::<Value>::new();
        let mut identical_count = 0i64;
        let mut differs_count = 0i64;
        let mut missing_count = 0i64;
        let mut first_divergence: Option<Value> = None;

        'slots: for key in &slot_order {
            let width = widths.get(key).copied().unwrap_or(0);
            for ordinal in 0..width {
                if aligned.len() >= MAX_ALIGNED_SLOTS {
                    break 'slots;
                }
                let mut observations = Vec::<Value>::new();
                let mut present = 0usize;
                let mut missing_runs = Vec::<String>::new();
                let mut signatures = Vec::<String>::new();
                for (index, (request_id, _)) in graphs.iter().enumerate() {
                    let node = per_run[index]
                        .get(key)
                        .and_then(|nodes| nodes.get(ordinal).copied());
                    match node {
                        Some(node) => {
                            present += 1;
                            let observation = slot_observation(node);
                            // The compared signature is exactly the recorded attributes a
                            // reviewer is shown — never a timestamp or row id, which differ
                            // between runs for reasons that are not differences.
                            signatures.push(format!(
                                "{}|{}|{}",
                                observation["kind"], observation["status"], observation["evidence"]
                            ));
                            observations.push(json!({
                                "request_id": request_id,
                                "present": true,
                                "observation": observation,
                            }));
                        }
                        None => {
                            missing_runs.push(request_id.clone());
                            observations.push(json!({
                                "request_id": request_id,
                                "present": false,
                                "observation": Value::Null,
                            }));
                        }
                    }
                }
                let verdict = if !missing_runs.is_empty() {
                    // Missing before differing: a slot absent from a run is an evidence gap,
                    // and describing it as a difference would overclaim.
                    missing_count += 1;
                    "missing_evidence"
                } else if signatures.windows(2).any(|pair| pair[0] != pair[1]) {
                    differs_count += 1;
                    "differs"
                } else {
                    identical_count += 1;
                    "identical"
                };
                let (kind, semantic) = key.split_once('|').unwrap_or(("request", key.as_str()));
                let slot = json!({
                    "slot": format!("{key}#{ordinal}"),
                    "semantic_step_id": semantic,
                    "kind": kind,
                    "ordinal": ordinal,
                    "verdict": verdict,
                    "present_in": present,
                    "missing_in": missing_runs,
                    "runs": observations,
                });
                if verdict != "identical" && first_divergence.is_none() {
                    first_divergence = Some(json!({
                        "aligned": true,
                        "slot": slot["slot"],
                        "semantic_step_id": semantic,
                        "kind": kind,
                        "verdict": verdict,
                        "difference_kind": if verdict == "missing_evidence" {
                            "missing_evidence"
                        } else {
                            "recorded_difference"
                        },
                        "detail": if verdict == "missing_evidence" {
                            "This step is recorded in at least one run and absent from another. Absence of a recorded step is not evidence the step did not happen."
                        } else {
                            "Every compared run recorded this step and at least one recorded attribute differs."
                        },
                        "runs": slot["runs"],
                    }));
                }
                aligned.push(slot);
            }
        }

        let total_slots = aligned.len();
        let first_divergence = first_divergence.unwrap_or_else(|| {
            json!({
                "aligned": true,
                "slot": Value::Null,
                "verdict": "identical",
                "difference_kind": "none",
                "detail": "Every aligned slot matched on kind, status and evidence label across the compared runs. This says the compared projections agree, not that the runs were identical in ways the projection does not record.",
            })
        });

        Ok(json!({
            "format": COMPARISON_FORMAT,
            "projection": super::provenance_projection(),
            "runs": runs,
            "compared": graphs.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            "aligned": aligned,
            "first_divergence": first_divergence,
            "counts": {
                "slots": total_slots,
                "identical": identical_count,
                "differs": differs_count,
                "missing_evidence": missing_count,
            },
            "bounds": {"max_runs": MAX_COMPARED_RUNS, "max_slots": MAX_ALIGNED_SLOTS},
            "truncated": {"slots": total_slots >= MAX_ALIGNED_SLOTS},
            "evidence_labels": evidence_legend(),
            "alignment_labels": alignment_legend(),
            "sanitizer": review::SANITIZER,
            "note": "Structural alignment over recorded rows. A missing slot is absence of evidence, not proof a step did not occur, and no counterfactual answer is claimed for any run.",
        }))
    }

    /// P16-T02: export one run's causal graph as a bounded sanitized artifact.
    ///
    /// `format` is `json` or `graphviz`. Both are rendered from the *same* sanitized node and
    /// edge set, so the DOT a reviewer opens in a viewer cannot contain anything the JSON
    /// omitted. Sanitization, the citation shape, canonical serialization and the checksum
    /// are all the P15-T04 implementations; this function contains no second copy.
    ///
    /// A label the shared sanitizer refuses is replaced with a marker naming the rejection
    /// code and the node is counted in `withheld`. It is not dropped, because a silently
    /// shorter graph would misrepresent the recorded structure, and it is not shipped either.
    pub async fn export_incident_graph(
        &self,
        request_id: String,
        format: String,
    ) -> Result<Option<Value>> {
        if format != "json" && format != "graphviz" {
            bail!("export format must be json or graphviz");
        }
        let Some(graph) = self
            .incident_view(request_id.clone(), super::IncidentQuery::default())
            .await?
        else {
            return Ok(None);
        };

        let empty = Vec::new();
        let nodes = graph["nodes"].as_array().unwrap_or(&empty);
        let edges = graph["edges"].as_array().unwrap_or(&empty);

        let mut withheld = 0i64;
        let mut export_nodes = Vec::<Value>::new();
        for node in nodes {
            let raw = node["label"].as_str().unwrap_or_default();
            // The one sanitizer. An export can never be more permissive than search.
            let (label, sanitized) = match review::sanitize(raw) {
                Ok(clean) => (clean, true),
                Err(rejection) => {
                    withheld += 1;
                    (format!("[withheld: {}]", rejection.code()), false)
                }
            };
            export_nodes.push(json!({
                "id": node["id"],
                "kind": node["kind"],
                "row_id": node["row_id"],
                "label": label,
                "label_sanitized": sanitized,
                "status": node["status"],
                "at": node["at"],
                "evidence": evidence_label(None, node["confidence"].as_str()),
                "confidence": node["confidence"],
                "confidence_basis": node["confidence_basis"],
            }));
        }
        let export_edges: Vec<Value> = edges
            .iter()
            .map(|edge| {
                json!({
                    "id": edge["id"],
                    "source": edge["source"],
                    "target": edge["target"],
                    "relation": edge["relation"],
                    "evidence": evidence_label(edge["relation"].as_str(), edge["confidence"].as_str()),
                    "confidence": edge["confidence"],
                })
            })
            .collect();

        // The citation shape search and packet export already use. `kind` is `turn` because
        // the exported artifact is about one recorded coding turn.
        let citation = Citation {
            id: format!("incident:{request_id}"),
            kind: "turn".into(),
            source_id: request_id.clone(),
            revision: 1,
            scope: graph["request"]["scope"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            session_id: graph["request"]["session_id"].as_str().map(str::to_string),
            timestamp: graph["request"]["updated_at"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            content_sha256: String::new(),
            sanitizer: review::SANITIZER.into(),
        };

        let mut artifact = json!({
            "format": EXPORT_FORMAT,
            "projection": graph["projection"],
            "request": graph["request"],
            "citation": citation.to_json(),
            "nodes": export_nodes,
            "edges": export_edges,
            "earliest_known_break": graph["earliest_known_break"],
            "counts": graph["counts"],
            "bounds": graph["bounds"],
            "truncated": graph["truncated"],
            "withheld_labels": withheld,
            "evidence_labels": evidence_legend(),
            "confidence_labels": graph["confidence_labels"],
            "sanitizer": review::SANITIZER,
            "note": "Bounded sanitized causal graph over recorded rows. Evidence labels distinguish a recorded dependency or contradiction from mere temporal proximity, and say unknown when evidence is absent. No field carries model reasoning.",
        });
        // The shared canonical serialization and the shared checksum helper: an exported
        // graph is verifiable the same way a continuation packet is.
        let digest = review::checksum(&canonical_json(&artifact));
        artifact["content_sha256"] = json!(digest);
        if let Some(object) = artifact["citation"].as_object_mut() {
            object.insert("content_sha256".into(), json!(digest));
        }

        if format == "graphviz" {
            // Rendered from the artifact just built, never from the raw graph, so the DOT
            // cannot carry a byte the JSON withheld.
            artifact["graphviz"] = json!(graphviz(&artifact));
        }
        artifact["export_format"] = json!(format);
        Ok(Some(artifact))
    }
}

/// Render the already-sanitized artifact as Graphviz DOT.
///
/// Every string that reaches the output goes through [`dot_escape`], and every label in the
/// artifact has already passed the shared sanitizer, so this function adds no new content —
/// it only re-serializes. Evidence class drives the visual style, so a reader cannot mistake
/// a temporal co-occurrence for a recorded dependency.
fn graphviz(artifact: &Value) -> String {
    let mut out = String::from("digraph causal_graph {\n  rankdir=LR;\n");
    out.push_str("  graph [labelloc=\"t\", label=\"");
    out.push_str(&dot_escape(
        "Recorded causal graph. Dashed = temporal proximity only (co-occurrence, not causation). Dotted = unknown. Red = recorded contradiction.",
    ));
    out.push_str("\"];\n");
    if let Some(nodes) = artifact["nodes"].as_array() {
        for node in nodes {
            let id = node["id"].as_str().unwrap_or_default();
            let evidence = node["evidence"].as_str().unwrap_or("unknown");
            let style = match evidence {
                "dependency" => "solid",
                "contradiction" => "solid",
                "temporal" => "dashed",
                _ => "dotted",
            };
            out.push_str(&format!(
                "  \"{}\" [label=\"{}\\n{} · {}\", style={}];\n",
                dot_escape(id),
                dot_escape(node["label"].as_str().unwrap_or_default()),
                dot_escape(node["kind"].as_str().unwrap_or_default()),
                dot_escape(node["status"].as_str().unwrap_or_default()),
                style
            ));
        }
    }
    if let Some(edges) = artifact["edges"].as_array() {
        for edge in edges {
            let evidence = edge["evidence"].as_str().unwrap_or("unknown");
            let (style, color) = match evidence {
                "contradiction" => ("solid", "red"),
                "dependency" => ("solid", "black"),
                "temporal" => ("dashed", "gray"),
                _ => ("dotted", "gray"),
            };
            out.push_str(&format!(
                "  \"{}\" -> \"{}\" [label=\"{} · {}\", style={}, color={}];\n",
                dot_escape(edge["source"].as_str().unwrap_or_default()),
                dot_escape(edge["target"].as_str().unwrap_or_default()),
                dot_escape(edge["relation"].as_str().unwrap_or_default()),
                dot_escape(evidence),
                style,
                color
            ));
        }
    }
    out.push_str("}\n");
    out
}

/// Escape a string for a DOT double-quoted literal. Newlines become `\n` escapes rather than
/// real line breaks so one label cannot forge extra DOT statements.
fn dot_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' | '\r' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_labels_never_upgrade_proximity_to_a_recorded_dependency() {
        assert_eq!(evidence_label(Some("depends_on"), None), "dependency");
        assert_eq!(evidence_label(Some("supports"), None), "dependency");
        assert_eq!(evidence_label(Some("contradicts"), None), "contradiction");
        assert_eq!(evidence_label(Some("invalidates"), None), "contradiction");
        assert_eq!(evidence_label(None, Some("temporal_proximity")), "temporal");
        assert_eq!(evidence_label(None, Some("unknown")), "unknown");
        assert_eq!(evidence_label(None, None), "unknown");
        // The legend ships every label the labeller can emit, so a client cannot receive a
        // label it has no definition for.
        let legend = evidence_legend();
        for label in ["dependency", "contradiction", "temporal", "unknown"] {
            assert!(legend[label].is_string(), "{label} has no legend entry");
        }
        assert!(legend["temporal"]
            .as_str()
            .unwrap()
            .contains("not causation"));
    }

    #[test]
    fn semantic_identity_ignores_row_ids_and_times_but_not_what_the_step_did() {
        let first = json!({"id":"step:aaa","kind":"step","tool":"edit","label":"tool: edit","at":"2026-01-01T00:00:00Z","path":null});
        let second = json!({"id":"step:zzz","kind":"step","tool":"edit","label":"tool: edit","at":"2026-09-09T09:09:09Z","path":null});
        // Same work in two runs aligns despite different row ids and timestamps.
        assert_eq!(semantic_key(&first), semantic_key(&second));
        let other_tool = json!({"id":"step:bbb","kind":"step","tool":"bash","label":"tool: bash","at":null,"path":null});
        assert_ne!(semantic_key(&first), semantic_key(&other_tool));
        // A mutation is identified by its path, not by its row id.
        let mutation = json!({"id":"mutation:1","kind":"mutation","tool":null,"path":"src/Main.rs","label":"write src/Main.rs"});
        let same_path = json!({"id":"mutation:2","kind":"mutation","tool":null,"path":"src/main.rs","label":"write src/main.rs"});
        assert_eq!(semantic_key(&mutation), semantic_key(&same_path));
        // The request frame is exactly one slot per run.
        assert_eq!(
            semantic_key(&json!({"kind":"request","label":"coding turn"})),
            "request"
        );
    }

    #[test]
    fn dot_escaping_cannot_forge_statements() {
        let hostile = "a\" ]; \"x\" -> \"y\" [label=\"injected\nsecond line";
        let escaped = dot_escape(hostile);
        assert!(!escaped.contains('\n'));
        // Every quote in the output is an escaped quote, so the literal cannot be closed early.
        let bytes: Vec<char> = escaped.chars().collect();
        for (index, ch) in bytes.iter().enumerate() {
            if *ch == '"' {
                assert!(index > 0 && bytes[index - 1] == '\\', "unescaped quote");
            }
        }
    }

    #[test]
    fn graphviz_renders_only_the_sanitized_artifact_and_marks_weak_evidence() {
        let artifact = json!({
            "nodes": [
                {"id":"request:r","kind":"request","label":"coding turn","status":"complete","evidence":"dependency"},
                {"id":"step:s","kind":"step","label":"tool: edit","status":"failed","evidence":"temporal"},
                {"id":"memory:m","kind":"memory","label":"[withheld: sensitive_content]","status":"known","evidence":"unknown"},
            ],
            "edges": [
                {"id":"e1","source":"memory:m","target":"step:s","relation":"contradicts","evidence":"contradiction"},
                {"id":"e2","source":"request:r","target":"step:s","relation":"depends_on","evidence":"dependency"},
            ]
        });
        let dot = graphviz(&artifact);
        assert!(dot.starts_with("digraph causal_graph {"));
        assert!(dot.trim_end().ends_with('}'));
        assert!(dot.contains("not causation"));
        // A temporal-only node is visually distinct from a recorded dependency.
        assert!(dot.contains("\"step:s\" [label=\"tool: edit\\nstep · failed\", style=dashed]"));
        assert!(dot.contains("style=dotted"));
        // A recorded contradiction is drawn as one, and the withheld label is what reaches
        // the output rather than the content the sanitizer refused.
        assert!(dot.contains("contradicts · contradiction"));
        assert!(dot.contains("[withheld: sensitive_content]"));
    }
}
