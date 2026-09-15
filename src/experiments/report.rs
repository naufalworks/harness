//! Sanitized, review-gated research reports for Memory Wind Tunnel experiments (P17-T05).
//!
//! Reports contain bounded statistical projections and content-addressed evidence links, never raw
//! prompts, memory bodies, provider transcripts, tool payloads, or project files. Free text passes
//! through the shared export sanitizer. Release is bound to the exact report digest reviewed.

use super::experiment::{EffectInterpretation, PairedSummary};
use crate::export::packet::canonical_json;
use crate::export::review;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const FORMAT: &str = "harness-research-report-v1";
pub const PREVIEW_FORMAT: &str = "harness-research-report-preview-v1";
pub const MAX_SUMMARIES: usize = 64;
pub const MAX_EVIDENCE: usize = 512;
pub const MAX_LIMITATIONS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Capsule,
    Treatment,
    ReplayReport,
    PairedSummary,
    TrialReceipt,
}

impl EvidenceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Capsule => "capsule",
            Self::Treatment => "treatment",
            Self::ReplayReport => "replay_report",
            Self::PairedSummary => "paired_summary",
            Self::TrialReceipt => "trial_receipt",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceAvailability {
    Available,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceReferenceInput {
    pub kind: EvidenceKind,
    pub stable_id: String,
    pub availability: EvidenceAvailability,
    pub content_sha256: Option<String>,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceReference {
    pub kind: EvidenceKind,
    pub stable_id: String,
    pub availability: EvidenceAvailability,
    pub content_sha256: Option<String>,
    pub evidence_uri: Option<String>,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportSummary {
    pub source_summary_id: String,
    pub treatment_id: String,
    pub total_pairs: u64,
    pub available_pairs: u64,
    pub unavailable_pairs: u64,
    pub baseline_success_rate: Option<f64>,
    pub treatment_success_rate: Option<f64>,
    pub mean_paired_effect: Option<f64>,
    pub ci95_low: Option<f64>,
    pub ci95_high: Option<f64>,
    pub interpretation: EffectInterpretation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchReportInput {
    pub experiment_id: String,
    pub title: String,
    pub question: String,
    pub summaries: Vec<PairedSummary>,
    pub evidence: Vec<EvidenceReferenceInput>,
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchReport {
    pub format: String,
    /// SHA-256 over canonical JSON with this field removed.
    pub report_id: String,
    pub experiment_id: String,
    pub title: String,
    pub question: String,
    pub sanitizer: String,
    pub summaries: Vec<ReportSummary>,
    pub evidence: Vec<EvidenceReference>,
    pub limitations: Vec<String>,
    pub withheld_values: u64,
    pub scope_note: String,
}

impl ResearchReport {
    fn canonical_without_id(&self) -> Result<String> {
        let mut value = serde_json::to_value(self)?;
        value
            .as_object_mut()
            .context("research report must serialize as an object")?
            .remove("report_id");
        Ok(canonical_json(&value))
    }

    fn seal(mut self) -> Result<Self> {
        self.report_id.clear();
        self.report_id = review::checksum(&self.canonical_without_id()?);
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<()> {
        if self.format != FORMAT || self.sanitizer != review::SANITIZER {
            bail!("unsupported research report format or sanitizer");
        }
        digest("report id", &self.report_id)?;
        if self.report_id != review::checksum(&self.canonical_without_id()?) {
            bail!("research report content address does not match its bytes");
        }
        safe_id("experiment id", &self.experiment_id)?;
        exact_sanitized("report title", &self.title)?;
        exact_sanitized("research question", &self.question)?;
        if self.summaries.is_empty() || self.summaries.len() > MAX_SUMMARIES {
            bail!("research report requires 1..=64 paired summaries");
        }
        if self.evidence.is_empty() || self.evidence.len() > MAX_EVIDENCE {
            bail!("research report requires 1..=512 evidence references");
        }
        if self.limitations.is_empty() || self.limitations.len() > MAX_LIMITATIONS {
            bail!("research report requires 1..=128 explicit limitations");
        }
        let mut summaries = BTreeSet::new();
        for summary in &self.summaries {
            digest("source summary id", &summary.source_summary_id)?;
            safe_id("summary treatment id", &summary.treatment_id)?;
            if !summaries.insert(summary.source_summary_id.as_str()) {
                bail!("research report summary ids must be unique");
            }
            if summary.available_pairs + summary.unavailable_pairs != summary.total_pairs {
                bail!("research report summary pair counts do not add up");
            }
        }
        let mut evidence = BTreeSet::new();
        for reference in &self.evidence {
            safe_id("evidence stable id", &reference.stable_id)?;
            if !evidence.insert((reference.kind, reference.stable_id.as_str())) {
                bail!("research report evidence references must be unique");
            }
            match reference.availability {
                EvidenceAvailability::Available => {
                    let hash = reference
                        .content_sha256
                        .as_deref()
                        .context("available evidence requires a content hash")?;
                    digest("evidence content hash", hash)?;
                    let expected = evidence_uri(reference.kind, &reference.stable_id, hash);
                    if reference.evidence_uri.as_deref() != Some(expected.as_str())
                        || reference.unavailable_reason.is_some()
                    {
                        bail!("available evidence requires its exact bounded evidence URI");
                    }
                }
                EvidenceAvailability::Unavailable => {
                    if reference.content_sha256.is_some() || reference.evidence_uri.is_some() {
                        bail!("unavailable evidence cannot pretend to have bytes or a link");
                    }
                    exact_sanitized(
                        "unavailable evidence reason",
                        reference
                            .unavailable_reason
                            .as_deref()
                            .context("unavailable evidence requires an explicit reason")?,
                    )?;
                }
            }
        }
        for limitation in &self.limitations {
            exact_sanitized("report limitation", limitation)?;
        }
        Ok(())
    }

    pub fn canonical_json(&self) -> Result<String> {
        self.validate()?;
        Ok(canonical_json(&serde_json::to_value(self)?))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchReportPreview {
    pub format: String,
    pub report_id: String,
    pub report: ResearchReport,
    pub note: String,
}

pub fn prepare_report(input: ResearchReportInput) -> Result<ResearchReportPreview> {
    safe_id("experiment id", &input.experiment_id)?;
    if input.summaries.is_empty() || input.summaries.len() > MAX_SUMMARIES {
        bail!("research report requires 1..=64 paired summaries");
    }
    if input.evidence.is_empty() || input.evidence.len() > MAX_EVIDENCE {
        bail!("research report requires 1..=512 evidence references");
    }
    if input.limitations.is_empty() || input.limitations.len() > MAX_LIMITATIONS - 2 {
        bail!("research report requires 1..=126 caller limitations");
    }

    let mut withheld = 0_u64;
    let title = clean_text(&input.title, &mut withheld);
    let question = clean_text(&input.question, &mut withheld);
    let mut summaries = Vec::with_capacity(input.summaries.len());
    let mut limitations = Vec::new();
    for summary in input.summaries {
        summary.canonical_json()?;
        for limitation in &summary.limitations {
            limitations.push(clean_text(limitation, &mut withheld));
        }
        summaries.push(ReportSummary {
            source_summary_id: summary.summary_id,
            treatment_id: summary.treatment_id,
            total_pairs: summary.total_pairs,
            available_pairs: summary.available_pairs,
            unavailable_pairs: summary.unavailable_pairs,
            baseline_success_rate: summary.baseline_success_rate,
            treatment_success_rate: summary.treatment_success_rate,
            mean_paired_effect: summary.mean_paired_effect,
            ci95_low: summary.ci95_low,
            ci95_high: summary.ci95_high,
            interpretation: summary.interpretation,
        });
    }
    for limitation in input.limitations {
        limitations.push(clean_text(&limitation, &mut withheld));
    }
    limitations.push(
        "The report links sanitized content-addressed evidence; it does not embed raw prompts, memory bodies, provider transcripts, tool payloads, or project files."
            .into(),
    );
    limitations.push(
        "Observed paired effects are descriptive for the declared capsule and treatment conditions and do not establish universal causality."
            .into(),
    );
    limitations.sort();
    limitations.dedup();
    if limitations.len() > MAX_LIMITATIONS {
        bail!("sanitized report limitations exceed 128 items");
    }

    let mut evidence = Vec::with_capacity(input.evidence.len());
    for reference in input.evidence {
        safe_id("evidence stable id", &reference.stable_id)?;
        let (content_sha256, evidence_uri, unavailable_reason) = match reference.availability {
            EvidenceAvailability::Available => {
                let hash = reference
                    .content_sha256
                    .context("available evidence requires a content hash")?;
                digest("evidence content hash", &hash)?;
                if reference.unavailable_reason.is_some() {
                    bail!("available evidence cannot also be unavailable");
                }
                let uri = evidence_uri(reference.kind, &reference.stable_id, &hash);
                (Some(hash), Some(uri), None)
            }
            EvidenceAvailability::Unavailable => {
                if reference.content_sha256.is_some() {
                    bail!("unavailable evidence cannot claim a content hash");
                }
                let reason = clean_text(
                    &reference
                        .unavailable_reason
                        .context("unavailable evidence requires an explicit reason")?,
                    &mut withheld,
                );
                (None, None, Some(reason))
            }
        };
        evidence.push(EvidenceReference {
            kind: reference.kind,
            stable_id: reference.stable_id,
            availability: reference.availability,
            content_sha256,
            evidence_uri,
            unavailable_reason,
        });
    }

    let report = ResearchReport {
        format: FORMAT.into(),
        report_id: String::new(),
        experiment_id: input.experiment_id,
        title,
        question,
        sanitizer: review::SANITIZER.into(),
        summaries,
        evidence,
        limitations,
        withheld_values: withheld,
        scope_note: "Bounded statistical projections and sanitized content-addressed evidence references only; raw experiment inputs and outputs are excluded."
            .into(),
    }
    .seal()?;
    Ok(ResearchReportPreview {
        format: PREVIEW_FORMAT.into(),
        report_id: report.report_id.clone(),
        report,
        note: "Nothing has left yet. Review this exact report ID before release.".into(),
    })
}

pub fn release_report(
    preview: ResearchReportPreview,
    reviewed_report_id: &str,
) -> Result<ResearchReport> {
    if preview.format != PREVIEW_FORMAT
        || preview.report_id != preview.report.report_id
        || preview.report_id != reviewed_report_id
    {
        bail!("research report review digest is stale or missing");
    }
    preview.report.validate()?;
    Ok(preview.report)
}

fn clean_text(text: &str, withheld: &mut u64) -> String {
    match review::sanitize(text) {
        Ok(cleaned) => {
            if cleaned != text {
                *withheld += 1;
            }
            cleaned
        }
        Err(reason) => {
            *withheld += 1;
            format!("[withheld: {}]", reason.code())
        }
    }
}

fn evidence_uri(kind: EvidenceKind, stable_id: &str, hash: &str) -> String {
    format!(
        "harness-evidence://{}/{stable_id}?sha256={hash}",
        kind.as_str()
    )
}

fn exact_sanitized(name: &str, value: &str) -> Result<()> {
    if value.is_empty() || !review::is_sanitized(value) {
        bail!("{name} must be non-empty text accepted unchanged by the shared sanitizer");
    }
    Ok(())
}

fn safe_id(name: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        bail!("{name} must be a bounded opaque identifier");
    }
    Ok(())
}

fn digest(name: &str, value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{name} must be a lowercase SHA-256 digest");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::experiments::experiment::{
        summarize_paired, MetricSource, PairedTrial, TrialOutcome,
    };

    fn outcome(pair: &str, treatment: &str, success: Option<bool>) -> TrialOutcome {
        TrialOutcome {
            pair_id: pair.into(),
            treatment_id: treatment.into(),
            success,
            success_source: success.map(|_| MetricSource::DeterministicAssertion),
            unavailable_reason: success
                .is_none()
                .then(|| "assertion output unavailable".into()),
            unsafe_actions: Some(0),
        }
    }

    fn input() -> ResearchReportInput {
        let treatment = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let summary = summarize_paired(
            treatment,
            &[
                PairedTrial {
                    pair_id: "pair-1".into(),
                    baseline: outcome("pair-1", "baseline", Some(true)),
                    treatment: outcome("pair-1", treatment, Some(false)),
                },
                PairedTrial {
                    pair_id: "pair-2".into(),
                    baseline: outcome("pair-2", "baseline", Some(true)),
                    treatment: outcome("pair-2", treatment, Some(false)),
                },
            ],
        )
        .unwrap();
        ResearchReportInput {
            experiment_id: "experiment-fixture-1".into(),
            title: "Memory treatment results".into(),
            question: "Did stale memory change deterministic success?".into(),
            summaries: vec![summary],
            evidence: vec![
                EvidenceReferenceInput {
                    kind: EvidenceKind::PairedSummary,
                    stable_id: "paired-summary-1".into(),
                    availability: EvidenceAvailability::Available,
                    content_sha256: Some(
                        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                    ),
                    unavailable_reason: None,
                },
                EvidenceReferenceInput {
                    kind: EvidenceKind::TrialReceipt,
                    stable_id: "trial-missing-1".into(),
                    availability: EvidenceAvailability::Unavailable,
                    content_sha256: None,
                    unavailable_reason: Some("provider usage evidence unavailable".into()),
                },
            ],
            limitations: vec!["Small deterministic fixture sample".into()],
        }
    }

    #[test]
    fn report_is_sanitized_content_addressed_and_review_gated() {
        let mut input = input();
        input.question.push_str("\napi_key=abcdef123456");
        let preview = prepare_report(input).unwrap();
        preview.report.validate().unwrap();
        let json = preview.report.canonical_json().unwrap();
        assert!(!json.contains("abcdef123456"));
        assert_eq!(preview.report.withheld_values, 1);
        assert!(json.contains("harness-evidence://paired_summary/paired-summary-1"));
        assert!(json.contains("provider usage evidence unavailable"));
        assert!(release_report(preview.clone(), "wrong").is_err());
        let released = release_report(preview.clone(), &preview.report_id).unwrap();
        assert_eq!(released.report_id, preview.report_id);
    }

    #[test]
    fn report_tampering_and_false_availability_are_refused() {
        let preview = prepare_report(input()).unwrap();
        let mut tampered = preview.report.clone();
        tampered.question = "Different question".into();
        assert!(tampered.validate().is_err());

        let mut bad = input();
        bad.evidence[1].availability = EvidenceAvailability::Available;
        assert!(prepare_report(bad).is_err());
    }
}
