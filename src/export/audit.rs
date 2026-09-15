//! P13-T04: a review-gated audit artifact for one selected run.
//!
//! The source is the already bounded P16 incident export. This module applies a second,
//! recursive sanitizer and copies only an explicit allow-list into the artifact. Review is
//! pinned to the bundle checksum: release requires the exact digest returned by preview and
//! recomputes the artifact, so a changed run cannot leave on the strength of a stale review.

use super::packet::canonical_json;
use super::review;
use anyhow::{bail, Result};
use serde_json::{json, Map, Value};

pub const FORMAT: &str = "harness-audit-bundle-v1";
const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn clean(value: &Value, withheld: &mut u64) -> Value {
    match value {
        Value::String(text) => match review::sanitize(text) {
            Ok(cleaned) => {
                if cleaned != *text {
                    *withheld += 1;
                }
                Value::String(cleaned)
            }
            Err(reason) => {
                *withheld += 1;
                Value::String(format!("[withheld: {}]", reason.code()))
            }
        },
        Value::Array(values) => Value::Array(values.iter().map(|v| clean(v, withheld)).collect()),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), clean(value, withheld)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn selected(source: &Value) -> Value {
    let keys = [
        "format",
        "projection",
        "request",
        "citation",
        "earliest_known_break",
        "counts",
        "bounds",
        "truncated",
        "withheld_labels",
        "evidence_labels",
        "confidence_labels",
        "sanitizer",
    ];
    let mut output = Map::new();
    for key in keys {
        if let Some(value) = source.get(key) {
            output.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(output)
}

fn record(kind: &str, stable_id: String, payload: Value) -> Value {
    let checksum = review::checksum(&canonical_json(&payload));
    json!({
        "kind": kind,
        "stable_id": stable_id,
        "payload": payload,
        "content_sha256": checksum,
        "previous_sha256": Value::Null,
        "chain_sha256": Value::Null,
    })
}

/// Build exactly what the reviewer sees. No unrelated database or workspace row is queried:
/// the caller supplies one selected run's sanitized incident artifact.
pub fn prepare(
    request_id: &str,
    audience: &str,
    hash_chain: bool,
    source: &Value,
) -> Result<Value> {
    if !matches!(audience, "self" | "team" | "public") {
        bail!("unsupported audit audience");
    }
    if source["citation"]["source_id"].as_str() != Some(request_id) {
        bail!("incident artifact does not belong to the selected run");
    }

    let mut withheld = 0u64;
    let mut records = vec![record(
        "run_summary",
        request_id.to_string(),
        clean(&selected(source), &mut withheld),
    )];
    for node in source["nodes"].as_array().into_iter().flatten() {
        records.push(record(
            "node",
            node["id"].as_str().unwrap_or_default().to_string(),
            clean(node, &mut withheld),
        ));
    }
    for edge in source["edges"].as_array().into_iter().flatten() {
        records.push(record(
            "edge",
            edge["id"].as_str().unwrap_or_default().to_string(),
            clean(edge, &mut withheld),
        ));
    }

    let mut previous = ZERO_HASH.to_string();
    if hash_chain {
        for item in &mut records {
            item["previous_sha256"] = json!(previous);
            let link = json!({
                "previous_sha256": item["previous_sha256"],
                "kind": item["kind"],
                "stable_id": item["stable_id"],
                "content_sha256": item["content_sha256"],
            });
            previous = review::checksum(&canonical_json(&link));
            item["chain_sha256"] = json!(previous);
        }
    }

    let mut bundle = json!({
        "format": FORMAT,
        "request_id": request_id,
        "audience": audience,
        "sanitizer": review::SANITIZER,
        "hash_chain": {
            "enabled": hash_chain,
            "algorithm": "sha256",
            "chain_tip_sha256": if hash_chain { Value::String(previous) } else { Value::Null },
        },
        "item_count": records.len(),
        "withheld_values": withheld,
        "items": records,
        "scope_note": "Only the selected run's bounded incident projection is included. No unrelated workspace data or exact original bytes are present.",
    });
    let checksum = review::checksum(&canonical_json(&bundle));
    bundle["content_sha256"] = json!(checksum);
    verify(&bundle)?;
    Ok(json!({
        "outcome": "preview",
        "content_sha256": checksum,
        "bundle": bundle,
        "note": "Nothing has left yet. Review this exact checksum, then release with it."
    }))
}

/// Verify every item, the optional chain, and the bundle checksum.
pub fn verify(bundle: &Value) -> Result<()> {
    if bundle["format"] != FORMAT {
        bail!("unsupported audit bundle format");
    }
    let expected = bundle["content_sha256"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("audit bundle has no checksum"))?;
    let mut unsigned = bundle.clone();
    unsigned
        .as_object_mut()
        .expect("bundle is an object")
        .remove("content_sha256");
    if review::checksum(&canonical_json(&unsigned)) != expected {
        bail!("audit bundle checksum does not match");
    }
    let chained = bundle["hash_chain"]["enabled"].as_bool().unwrap_or(false);
    let mut previous = ZERO_HASH.to_string();
    let items = bundle["items"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("audit bundle has no items"))?;
    if items.len() != bundle["item_count"].as_u64().unwrap_or(u64::MAX) as usize {
        bail!("audit bundle item count does not match");
    }
    for item in items {
        let payload_hash = review::checksum(&canonical_json(&item["payload"]));
        if item["content_sha256"].as_str() != Some(payload_hash.as_str()) {
            bail!("audit item checksum does not match");
        }
        if chained {
            if item["previous_sha256"].as_str() != Some(previous.as_str()) {
                bail!("audit hash chain has a broken parent");
            }
            let link = json!({
                "previous_sha256": item["previous_sha256"],
                "kind": item["kind"],
                "stable_id": item["stable_id"],
                "content_sha256": item["content_sha256"],
            });
            previous = review::checksum(&canonical_json(&link));
            if item["chain_sha256"].as_str() != Some(previous.as_str()) {
                bail!("audit hash chain link does not match");
            }
        }
    }
    if chained && bundle["hash_chain"]["chain_tip_sha256"].as_str() != Some(previous.as_str()) {
        bail!("audit hash chain tip does not match");
    }
    Ok(())
}

/// Turn a preview into the released response only when the caller echoes the exact digest they
/// reviewed. The bundle is verified again immediately before it leaves.
pub fn release(preview: Value, reviewed_sha256: &str) -> Result<Value> {
    let bundle = preview["bundle"].clone();
    verify(&bundle)?;
    if bundle["content_sha256"].as_str() != Some(reviewed_sha256) {
        bail!("audit review digest is stale or missing");
    }
    Ok(json!({
        "outcome": "released",
        "reviewed_content_sha256": reviewed_sha256,
        "bundle": bundle,
        "note": "Reviewed, bounded and sanitized evidence for one selected run."
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reseal(bundle: &mut Value) {
        bundle.as_object_mut().unwrap().remove("content_sha256");
        bundle["content_sha256"] = json!(review::checksum(&canonical_json(bundle)));
    }

    fn source() -> Value {
        json!({
            "format":"harness-causal-graph-v1",
            "projection":"causal-neighborhood-v1",
            "request":{"request_id":"11111111-1111-4111-8111-111111111111","scope":"proj"},
            "citation":{"source_id":"11111111-1111-4111-8111-111111111111"},
            "nodes":[{"id":"request:1","label":"safe"},{"id":"step:2","label":"keep\napi_key=abcdef123456"}],
            "edges":[{"id":"edge:1","source":"request:1","target":"step:2","relation":"depends_on"}],
            "counts":{"nodes":2,"edges":1},
            "bounds":{"max_nodes":400,"max_edges":2000},
            "unrelated_workspace_data":{"secret":"must not leave"}
        })
    }

    #[test]
    fn audit_export_is_selected_sanitized_checksummed_and_review_gated() {
        let preview = prepare(
            "11111111-1111-4111-8111-111111111111",
            "self",
            true,
            &source(),
        )
        .unwrap();
        let bundle = &preview["bundle"];
        verify(bundle).unwrap();
        let text = canonical_json(bundle);
        assert!(!text.contains("abcdef123456"));
        assert!(!text.contains("unrelated_workspace_data"));
        assert_eq!(bundle["withheld_values"], 1);
        assert!(bundle["hash_chain"]["chain_tip_sha256"].is_string());
        assert!(release(preview.clone(), "0").is_err());
        let digest = preview["content_sha256"].as_str().unwrap().to_string();
        assert_eq!(release(preview, &digest).unwrap()["outcome"], "released");
    }

    #[test]
    fn audit_export_detects_payload_and_chain_tampering() {
        let preview = prepare(
            "11111111-1111-4111-8111-111111111111",
            "team",
            true,
            &source(),
        )
        .unwrap();
        let mut tampered = preview["bundle"].clone();
        tampered["items"][0]["payload"]["request"]["scope"] = json!("other");
        reseal(&mut tampered);
        assert!(verify(&tampered).is_err());
        let mut broken = preview["bundle"].clone();
        broken["items"][1]["previous_sha256"] = json!(ZERO_HASH);
        reseal(&mut broken);
        assert!(verify(&broken).is_err());

        let mut reordered = preview["bundle"].clone();
        reordered["items"].as_array_mut().unwrap().swap(1, 2);
        reseal(&mut reordered);
        assert!(verify(&reordered).is_err());

        let mut removed = preview["bundle"].clone();
        removed["items"].as_array_mut().unwrap().remove(1);
        removed["item_count"] = json!(removed["items"].as_array().unwrap().len());
        reseal(&mut removed);
        assert!(verify(&removed).is_err());
    }

    #[test]
    fn audit_export_without_chain_is_explicit_and_verifiable() {
        let preview = prepare(
            "11111111-1111-4111-8111-111111111111",
            "public",
            false,
            &source(),
        )
        .unwrap();
        let bundle = &preview["bundle"];
        assert_eq!(bundle["hash_chain"]["enabled"], false);
        assert!(bundle["hash_chain"]["chain_tip_sha256"].is_null());
        assert!(bundle["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| { item["previous_sha256"].is_null() && item["chain_sha256"].is_null() }));
        verify(bundle).unwrap();
    }
}
