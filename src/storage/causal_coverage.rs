//! P16-T03: measuring causal coverage, and recording a deployment as a first-class event with
//! its own causal trail.
//!
//! ## Why the metrics are computed and the deployments are stored
//!
//! Missing-edge, earliest-break and graph-size metrics are pure functions of rows that already
//! exist — `provenance_edges` plus the bounded projection over them. Persisting them would be
//! a cache with no invalidation story, which is the same reasoning P16-T01 recorded when it
//! declined to persist its projection. So they are computed on read, through
//! [`DbStore::incident_view`], the one projection.
//!
//! A deployment is the opposite case. Once the process is gone, nothing in the database can
//! reconstruct which commit was promoted at which second by which binary hash, so migration
//! 015 records it as it happens. `parent_id` records which phase a phase actually followed —
//! a real recorded reference, never an inference from adjacent timestamps, which is the same
//! distinction `provenance_edges` draws for a coding turn.
//!
//! Reviewer time is stored for the same reason: it is an observation about a human, and no
//! amount of graph can be re-read to recover it.
//!
//! ## What is claimed, and what is not
//!
//! `coverage` reports what fraction of recorded rows carry a recorded provenance edge. A row
//! without one is counted as *unmeasured evidence*, never as a row that had no cause. An
//! `earliest_break` of `unknown` means no break was recorded — not that the run succeeded.
//! An anomaly flag is `null` when the question was never asked, which is deliberately distinct
//! from `false`, meaning it was asked and the answer was no.
//!
//! Nothing here claims an outcome would have differed, and no field carries model reasoning.

use super::{now, uid, DbStore};
use anyhow::{bail, Result};
use rusqlite::params;
use serde_json::{json, Value};

/// The largest number of requests one coverage report will measure. Each costs a full bounded
/// projection read, so a report is a bounded reviewer artifact rather than a table scan.
pub const MAX_COVERAGE_REQUESTS: usize = 50;
/// The largest number of deployments one provenance read returns.
pub const MAX_DEPLOYMENTS: usize = 50;
/// The coverage report's format identity, so a consumer (P13-T04's audit bundles) pins a shape
/// rather than guessing at one.
pub const COVERAGE_FORMAT: &str = "causal-coverage-v1";

/// The five phases of a deployment's recorded trail, and the vocabulary migration 015 enforces.
pub const DEPLOYMENT_PHASES: [&str; 5] = ["build", "restart", "smoke", "outcome", "rollback"];
/// The four recorded phase results. `unknown` exists so a phase whose result was never observed
/// is recordable as exactly that.
pub const DEPLOYMENT_STATUSES: [&str; 4] = ["started", "succeeded", "failed", "unknown"];

/// What each metric is allowed to mean. Shipped with the report so a consumer cannot invent a
/// stronger reading of a number than the number supports.
fn coverage_legend() -> Value {
    json!({
        "edge_coverage": "Fraction of selected nodes that carry at least one recorded provenance edge. The remainder is unmeasured evidence, not evidence that those rows had no cause.",
        "missing_edges": "Count of selected nodes with no recorded provenance edge. Each is an evidence gap in the recording, not a claim about what happened.",
        "earliest_break": "The earliest recorded failure, denial or recovery in the run. `unknown` means none was recorded, which is not the same as the run having succeeded.",
        "graph_size": "Nodes and edges actually returned by the bounded projection, with what the bound omitted reported separately.",
        "reviewer_time": "Measured wall-clock duration of closed reviewer sessions over this incident. Open and abandoned reviews are counted separately and never assigned an assumed duration.",
        "anomaly_flags": "Recorded observations about a deployment phase. `null` means the question was never asked, which is distinct from `false`."
    })
}

/// The anomaly flags recorded about one deployment phase.
///
/// Grouped into a struct rather than passed as four loose booleans so a caller cannot silently
/// transpose two of them, and so `None` — the question was never asked — stays visibly distinct
/// from `Some(false)`, the question was asked and the answer was no.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeploymentAnomalies {
    pub identity_mismatch: Option<bool>,
    pub unready: Option<bool>,
    pub smoke_failed: Option<bool>,
    pub schema_regressed: Option<bool>,
}

impl DbStore {
    /// P16-T03: measure causal coverage for one request, through the one projection.
    ///
    /// Every number here is derived from the same `incident_view` a reviewer opens, so a
    /// coverage figure can never describe a graph the reviewer cannot see.
    pub async fn causal_coverage(&self, request_id: String) -> Result<Option<Value>> {
        let Some(graph) = self
            .incident_view(request_id.clone(), super::IncidentQuery::default())
            .await?
        else {
            return Ok(None);
        };
        let empty = Vec::new();
        let nodes = graph["nodes"].as_array().unwrap_or(&empty);
        let total = nodes.len() as i64;
        // A node "has an edge" only when the projection recorded one for it. Proximity does
        // not count toward coverage: counting it would make the metric report a level of
        // recorded provenance that does not exist.
        let with_edge = nodes
            .iter()
            .filter(|node| node["confidence"] == json!("recorded_dependency"))
            .count() as i64;
        let proximity = nodes
            .iter()
            .filter(|node| node["confidence"] == json!("temporal_proximity"))
            .count() as i64;
        let unknown = total - with_edge - proximity;
        let missing_edges = total - with_edge;
        // Integer-safe ratio, reported to three decimals and only when there is something to
        // divide. A coverage of "1.0" over zero nodes would be a false reassurance.
        let coverage = if total > 0 {
            json!(((with_edge as f64 / total as f64) * 1000.0).round() / 1000.0)
        } else {
            Value::Null
        };

        let break_node = &graph["earliest_known_break"];
        let recorded_break = break_node["known"] == json!(true);

        let review = self.reviewer_time(request_id.clone()).await?;

        Ok(Some(json!({
            "format": COVERAGE_FORMAT,
            "request_id": request_id,
            "projection": graph["projection"],
            "graph_size": {
                "nodes": graph["counts"]["nodes"],
                "edges": graph["counts"]["edges"],
                "bounds": graph["bounds"],
                "truncated": graph["truncated"],
            },
            "edge_coverage": coverage,
            "missing_edges": missing_edges,
            "evidence_breakdown": {
                "recorded_dependency": with_edge,
                "temporal_proximity": proximity,
                "unknown": unknown,
            },
            "earliest_break": if recorded_break {
                json!({
                    "recorded": true,
                    "node_id": break_node["node_id"],
                    "kind": break_node["kind"],
                    "reason": break_node["reason"],
                })
            } else {
                json!({
                    "recorded": false,
                    "node_id": Value::Null,
                    "kind": "unknown",
                    "reason": "no recorded failure, denial or recovery break was found; this is an absence of evidence, not evidence the run succeeded",
                })
            },
            "reviewer_time": review,
            "metric_labels": coverage_legend(),
            "note": "Coverage over recorded rows, measured through the same bounded projection a reviewer opens. A row without a recorded edge is an evidence gap, never a row proven to have no cause.",
        })))
    }

    /// Aggregate coverage across several requests, for an audit-evidence view.
    ///
    /// Shaped for reuse as audit evidence (P13-T04 depends on this task): every per-request
    /// figure is retained beside the aggregate, so a bundle can cite the individual
    /// measurement rather than only a rolled-up number nobody can trace back.
    pub async fn causal_coverage_summary(&self, request_ids: Vec<String>) -> Result<Value> {
        if request_ids.is_empty() {
            bail!("coverage needs at least one request id");
        }
        if request_ids.len() > MAX_COVERAGE_REQUESTS {
            bail!("coverage accepts at most {MAX_COVERAGE_REQUESTS} request ids");
        }
        let mut reports = Vec::<Value>::new();
        let mut measured = 0i64;
        let mut absent = 0i64;
        let mut total_nodes = 0i64;
        let mut total_with_edge = 0i64;
        let mut breaks_recorded = 0i64;
        for id in request_ids {
            match self.causal_coverage(id.clone()).await? {
                Some(report) => {
                    measured += 1;
                    let nodes = report["graph_size"]["nodes"]["returned"]
                        .as_i64()
                        .unwrap_or_default();
                    total_nodes += nodes;
                    total_with_edge += report["evidence_breakdown"]["recorded_dependency"]
                        .as_i64()
                        .unwrap_or_default();
                    if report["earliest_break"]["recorded"] == json!(true) {
                        breaks_recorded += 1;
                    }
                    reports.push(report);
                }
                None => {
                    absent += 1;
                    reports.push(json!({
                        "request_id": id,
                        "measured": false,
                        "detail": "No recording receipt exists for this request id, so nothing was measured for it. It is excluded from the aggregate rather than counted as zero coverage.",
                    }));
                }
            }
        }
        let aggregate_coverage = if total_nodes > 0 {
            json!(((total_with_edge as f64 / total_nodes as f64) * 1000.0).round() / 1000.0)
        } else {
            Value::Null
        };
        Ok(json!({
            "format": COVERAGE_FORMAT,
            "requests": reports,
            "counts": {
                "requested": measured + absent,
                "measured": measured,
                "not_recorded": absent,
            },
            "aggregate": {
                "nodes": total_nodes,
                "nodes_with_recorded_edge": total_with_edge,
                "missing_edges": total_nodes - total_with_edge,
                "edge_coverage": aggregate_coverage,
                "requests_with_recorded_break": breaks_recorded,
                "requests_without_recorded_break": measured - breaks_recorded,
            },
            "metric_labels": coverage_legend(),
            "note": "Aggregate over measured requests only. A request with no recording is reported as not measured rather than folded in as zero, because averaging in an absence would understate coverage of what was actually recorded.",
        }))
    }

    /// Measured reviewer time over one incident. Open and abandoned reviews are counted, never
    /// assigned a duration.
    pub async fn reviewer_time(&self, request_id: String) -> Result<Value> {
        self.read(move |c| {
            let (closed, total_ms, max_ms): (i64, Option<i64>, Option<i64>) = c.query_row(
                "SELECT count(*),sum(duration_ms),max(duration_ms) FROM incident_reviews WHERE request_id=?1 AND closed_at IS NOT NULL",
                [&request_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let open: i64 = c.query_row(
                "SELECT count(*) FROM incident_reviews WHERE request_id=?1 AND closed_at IS NULL",
                [&request_id],
                |r| r.get(0),
            )?;
            let abandoned: i64 = c.query_row(
                "SELECT count(*) FROM incident_reviews WHERE request_id=?1 AND outcome='abandoned'",
                [&request_id],
                |r| r.get(0),
            )?;
            let identified: i64 = c.query_row(
                "SELECT count(*) FROM incident_reviews WHERE request_id=?1 AND outcome='cause_identified'",
                [&request_id],
                |r| r.get(0),
            )?;
            Ok(json!({
                "closed_reviews": closed,
                "open_reviews": open,
                "abandoned_reviews": abandoned,
                "cause_identified_reviews": identified,
                // Null rather than 0 when nothing was measured: a mean of zero would read as
                // "reviews took no time" instead of "no review has been measured".
                "median_ms": Value::Null,
                "mean_ms": match (closed, total_ms) {
                    (0, _) | (_, None) => Value::Null,
                    (n, Some(sum)) => json!(sum / n),
                },
                "max_ms": max_ms.map(|v| json!(v)).unwrap_or(Value::Null),
                "measured": closed > 0,
            }))
        })
        .await
    }

    /// Open a reviewer session over one incident. The graph size is recorded with it, because a
    /// duration is only interpretable next to how much graph the reviewer was shown.
    pub async fn open_incident_review(
        &self,
        request_id: String,
        view: String,
        node_count: i64,
        edge_count: i64,
    ) -> Result<String> {
        if !["causal", "chronological", "comparison", "export"].contains(&view.as_str()) {
            bail!("unsupported incident review view");
        }
        if node_count < 0 || edge_count < 0 {
            bail!("graph size cannot be negative");
        }
        self.run(move |c| {
            let id = uid();
            c.execute(
                "INSERT INTO incident_reviews(id,request_id,view,node_count,edge_count,opened_at) VALUES(?1,?2,?3,?4,?5,?6)",
                params![id, request_id, view, node_count, edge_count, now()],
            )?;
            Ok(id)
        })
        .await
    }

    /// Close a reviewer session with a measured duration and a recorded outcome.
    ///
    /// `unknown` is an accepted outcome on purpose: a reviewer who found no cause must be able
    /// to say so rather than pick one, and a coverage report that counted every closed review
    /// as a success would misrepresent the tool's usefulness.
    pub async fn close_incident_review(
        &self,
        id: String,
        duration_ms: i64,
        outcome: String,
    ) -> Result<bool> {
        if !["cause_identified", "unknown", "abandoned"].contains(&outcome.as_str()) {
            bail!("unsupported incident review outcome");
        }
        if duration_ms < 0 {
            bail!("a measured duration cannot be negative");
        }
        self.run(move |c| {
            let changed = c.execute(
                "UPDATE incident_reviews SET closed_at=?1,duration_ms=?2,outcome=?3 WHERE id=?4 AND closed_at IS NULL",
                params![now(), duration_ms, outcome, id],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    /// Record one phase of a deployment.
    ///
    /// `parent_id` is the recorded dependency on the phase this one actually followed. It is
    /// never inferred: a caller that does not know which phase preceded this one passes `None`
    /// rather than guessing at the most recent row, because a guessed trail is worse than an
    /// unlinked one.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_deployment_event(
        &self,
        deployment_id: String,
        parent_id: Option<String>,
        phase: String,
        status: String,
        commit_sha: Option<String>,
        binary_sha256: Option<String>,
        schema_version: Option<i64>,
        detail: Option<String>,
    ) -> Result<String> {
        if !DEPLOYMENT_PHASES.contains(&phase.as_str()) {
            bail!("unsupported deployment phase");
        }
        if !DEPLOYMENT_STATUSES.contains(&status.as_str()) {
            bail!("unsupported deployment phase status");
        }
        // Detail is operator-facing text about a build or a smoke test, so it is redacted
        // before it is ever persisted, and bounded to the column's contract.
        let detail = detail.map(|text| {
            crate::safety::redact(&text)
                .chars()
                .take(2000)
                .collect::<String>()
        });
        let started = now();
        // A phase recorded as already-resolved carries its finish time in the same row; a
        // phase recorded as `started` has none, which is what migration 015's CHECK requires.
        let finished = (status != "started").then(|| started.clone());
        self.run(move |c| {
            let id = uid();
            c.execute(
                "INSERT INTO deployment_events(id,deployment_id,parent_id,phase,status,commit_sha,binary_sha256,schema_version,detail,started_at,finished_at)\
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![id, deployment_id, parent_id, phase, status, commit_sha, binary_sha256, schema_version, detail, started, finished],
            )?;
            Ok(id)
        })
        .await
    }

    /// Complete a phase that was recorded as `started`, with its result and its anomaly flags.
    ///
    /// Each flag is `Option<bool>`: `None` leaves it unasked rather than answering "no" on the
    /// caller's behalf. Migration 015's trigger allows exactly this update and refuses any
    /// rewrite of the phase's identity or history.
    pub async fn finish_deployment_event(
        &self,
        id: String,
        status: String,
        detail: Option<String>,
        anomalies: DeploymentAnomalies,
    ) -> Result<bool> {
        if !["succeeded", "failed", "unknown"].contains(&status.as_str()) {
            bail!("a finished deployment phase must be succeeded, failed or unknown");
        }
        let detail = detail.map(|text| {
            crate::safety::redact(&text)
                .chars()
                .take(2000)
                .collect::<String>()
        });
        let flag = |value: Option<bool>| value.map(i64::from);
        self.run(move |c| {
            let changed = c.execute(
                "UPDATE deployment_events SET status=?1,finished_at=?2,detail=COALESCE(?3,detail),\
                 anomaly_identity_mismatch=COALESCE(?4,anomaly_identity_mismatch),\
                 anomaly_unready=COALESCE(?5,anomaly_unready),\
                 anomaly_smoke_failed=COALESCE(?6,anomaly_smoke_failed),\
                 anomaly_schema_regressed=COALESCE(?7,anomaly_schema_regressed)\
                 WHERE id=?8 AND status='started'",
                params![
                    status,
                    now(),
                    detail,
                    flag(anomalies.identity_mismatch),
                    flag(anomalies.unready),
                    flag(anomalies.smoke_failed),
                    flag(anomalies.schema_regressed),
                    id
                ],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    /// Read one deployment's recorded causal trail, or the most recent deployments.
    ///
    /// Each phase reports the phase it recorded as its parent. A phase with no parent says so
    /// rather than being attached to whatever ran before it.
    pub async fn deployment_provenance(&self, deployment_id: Option<String>) -> Result<Value> {
        self.read(move |c| {
            let mut events = Vec::<Value>::new();
            let render = |r: &rusqlite::Row| -> rusqlite::Result<Value> {
                let flag = |value: Option<i64>| match value {
                    Some(1) => json!(true),
                    Some(0) => json!(false),
                    // Never asked. Distinct from `false`, which means asked and answered no.
                    _ => Value::Null,
                };
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "deployment_id": r.get::<_, String>(1)?,
                    "parent_id": r.get::<_, Option<String>>(2)?,
                    "phase": r.get::<_, String>(3)?,
                    "status": r.get::<_, String>(4)?,
                    "commit": r.get::<_, Option<String>>(5)?,
                    "binary_sha256": r.get::<_, Option<String>>(6)?,
                    "schema_version": r.get::<_, Option<i64>>(7)?,
                    "detail": r.get::<_, Option<String>>(8)?,
                    "started_at": r.get::<_, String>(9)?,
                    "finished_at": r.get::<_, Option<String>>(10)?,
                    "anomalies": {
                        "identity_mismatch": flag(r.get::<_, Option<i64>>(11)?),
                        "unready": flag(r.get::<_, Option<i64>>(12)?),
                        "smoke_failed": flag(r.get::<_, Option<i64>>(13)?),
                        "schema_regressed": flag(r.get::<_, Option<i64>>(14)?),
                    },
                }))
            };
            let columns = "id,deployment_id,parent_id,phase,status,commit_sha,binary_sha256,schema_version,detail,started_at,finished_at,\
                           anomaly_identity_mismatch,anomaly_unready,anomaly_smoke_failed,anomaly_schema_regressed";
            match &deployment_id {
                Some(target) => {
                    let sql = format!("SELECT {columns} FROM deployment_events WHERE deployment_id=?1 ORDER BY seq LIMIT ?2");
                    let mut stmt = c.prepare(&sql)?;
                    for row in stmt.query_map(params![target, MAX_DEPLOYMENTS as i64 * 8], render)? {
                        events.push(row?);
                    }
                }
                None => {
                    let sql = format!("SELECT {columns} FROM deployment_events ORDER BY seq DESC LIMIT ?1");
                    let mut stmt = c.prepare(&sql)?;
                    for row in stmt.query_map(params![MAX_DEPLOYMENTS as i64 * 8], render)? {
                        events.push(row?);
                    }
                    events.reverse();
                }
            }
            // Anomalies are surfaced as a list so an operator does not have to scan every
            // phase to find the one that went wrong.
            let flagged: Vec<Value> = events
                .iter()
                .filter(|event| {
                    event["anomalies"]
                        .as_object()
                        .is_some_and(|flags| flags.values().any(|value| value == &json!(true)))
                })
                .map(|event| {
                    json!({
                        "id": event["id"],
                        "deployment_id": event["deployment_id"],
                        "phase": event["phase"],
                        "status": event["status"],
                        "anomalies": event["anomalies"],
                    })
                })
                .collect();
            let unresolved = events
                .iter()
                .filter(|event| event["status"] == json!("started") || event["status"] == json!("unknown"))
                .count() as i64;
            Ok(json!({
                "deployment_id": deployment_id,
                "events": events,
                "anomaly_flags": flagged,
                "counts": {
                    "events": events.len(),
                    "anomalies": flagged.len(),
                    "unresolved_or_unknown": unresolved,
                },
                "phases": DEPLOYMENT_PHASES,
                "metric_labels": coverage_legend(),
                "bounds": {"max_deployments": MAX_DEPLOYMENTS},
                "note": "Recorded deployment provenance. A parent link is a recorded dependency between phases, never inferred from timestamps, and an anomaly flag of null means the question was never asked rather than answered no.",
            }))
        })
        .await
    }
}
