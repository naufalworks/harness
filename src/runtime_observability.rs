//! P16-T04: bounded, content-free runtime timing for one recorded turn.
//!
//! This module intentionally cannot accept prompts, responses, paths, tool arguments, or error
//! strings. Callers can record only fixed stage durations, fixed counters, a fixed terminal
//! outcome, and an explicit list of measurements this first slice does not yet provide.

use serde::Serialize;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

pub const SCHEMA: &str = "harness.runtime/v1";

#[derive(Clone, Default)]
pub struct RuntimeObserver {
    inner: Arc<Mutex<Counters>>,
}

#[derive(Default)]
struct Counters {
    context_ms: u64,
    provider_ms: u64,
    tool_ms: u64,
    permission_ms: u64,
    verification_ms: u64,
    publication_ms: u64,
    provider_calls: u64,
    tool_calls: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Durations {
    pub total_ms: u64,
    pub context_ms: u64,
    pub provider_ms: u64,
    pub tool_ms: u64,
    pub permission_ms: u64,
    pub verification_ms: u64,
    pub publication_ms: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Counts {
    pub provider_calls: u64,
    pub tool_calls: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RuntimeReport {
    pub schema: &'static str,
    pub event: &'static str,
    pub outcome: &'static str,
    pub durations: Durations,
    pub counts: Counts,
    pub unavailable: Vec<&'static str>,
}

#[cfg(test)]
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Distribution {
    pub sample_count: usize,
    pub median_ms: u64,
    pub p95_ms: u64,
    pub max_ms: u64,
}

impl RuntimeObserver {
    fn add(value: &mut u64, elapsed: Duration) {
        *value = value.saturating_add(elapsed.as_millis().min(u128::from(u64::MAX)) as u64);
    }

    pub fn context(&self, elapsed: Duration) {
        Self::add(
            &mut self.inner.lock().expect("runtime timing mutex").context_ms,
            elapsed,
        );
    }

    pub fn provider(&self, elapsed: Duration) {
        let mut inner = self.inner.lock().expect("runtime timing mutex");
        Self::add(&mut inner.provider_ms, elapsed);
        inner.provider_calls = inner.provider_calls.saturating_add(1);
    }

    pub fn tool(&self, elapsed: Duration) {
        let mut inner = self.inner.lock().expect("runtime timing mutex");
        Self::add(&mut inner.tool_ms, elapsed);
        inner.tool_calls = inner.tool_calls.saturating_add(1);
    }

    pub fn permission(&self, elapsed: Duration) {
        Self::add(
            &mut self
                .inner
                .lock()
                .expect("runtime timing mutex")
                .permission_ms,
            elapsed,
        );
    }

    pub fn verification(&self, elapsed: Duration) {
        Self::add(
            &mut self
                .inner
                .lock()
                .expect("runtime timing mutex")
                .verification_ms,
            elapsed,
        );
    }

    pub fn publication(&self, elapsed: Duration) {
        Self::add(
            &mut self
                .inner
                .lock()
                .expect("runtime timing mutex")
                .publication_ms,
            elapsed,
        );
    }

    pub fn report(&self, total: Duration, outcome: &'static str) -> RuntimeReport {
        let inner = self.inner.lock().expect("runtime timing mutex");
        RuntimeReport {
            schema: SCHEMA,
            event: "turn_runtime_finished",
            outcome,
            durations: Durations {
                total_ms: total.as_millis().min(u128::from(u64::MAX)) as u64,
                context_ms: inner.context_ms,
                provider_ms: inner.provider_ms,
                tool_ms: inner.tool_ms,
                permission_ms: inner.permission_ms,
                verification_ms: inner.verification_ms,
                publication_ms: inner.publication_ms,
            },
            counts: Counts {
                provider_calls: inner.provider_calls,
                tool_calls: inner.tool_calls,
            },
            unavailable: vec![
                "sqlite_queue_ms",
                "sqlite_read_ms",
                "sqlite_write_ms",
                "sqlite_commit_ms",
            ],
        }
    }

    pub fn emit(&self, total: Duration, outcome: &'static str) {
        if let Ok(line) = serde_json::to_string(&self.report(total, outcome)) {
            eprintln!("{line}");
        }
    }
}

#[cfg(test)]
pub fn distribution(samples: &[Duration]) -> Option<Distribution> {
    if samples.is_empty() {
        return None;
    }
    let mut values = samples
        .iter()
        .map(|value| value.as_millis().min(u128::from(u64::MAX)) as u64)
        .collect::<Vec<_>>();
    values.sort_unstable();
    let median_ms = values[(values.len() - 1) / 2];
    let p95_index = (values.len() * 95).div_ceil(100).saturating_sub(1);
    Some(Distribution {
        sample_count: values.len(),
        median_ms,
        p95_ms: values[p95_index],
        max_ms: *values.last().expect("non-empty samples"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_observability_report_is_bounded_and_content_free() {
        let observer = RuntimeObserver::default();
        observer.context(Duration::from_millis(3));
        observer.provider(Duration::from_millis(7));
        observer.tool(Duration::from_millis(5));
        observer.permission(Duration::from_millis(2));
        observer.verification(Duration::from_millis(4));
        observer.publication(Duration::from_millis(1));
        let report = observer.report(Duration::from_millis(30), "complete");
        let value = serde_json::to_value(report).unwrap();
        assert_eq!(value["schema"], SCHEMA);
        assert_eq!(value["durations"]["total_ms"], 30);
        assert_eq!(value["counts"]["provider_calls"], 1);
        assert_eq!(value["counts"]["tool_calls"], 1);
        let serialized = value.to_string();
        for forbidden in [
            "prompt",
            "response",
            "token",
            "authorization",
            "api_key",
            "path",
            "arguments",
            "output",
            "reasoning",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "leaked forbidden field {forbidden}"
            );
        }
        assert_eq!(value["unavailable"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn runtime_observability_distribution_reports_repeated_samples() {
        let samples = [10, 20, 30, 40, 100].map(Duration::from_millis);
        assert_eq!(
            distribution(&samples),
            Some(Distribution {
                sample_count: 5,
                median_ms: 30,
                p95_ms: 100,
                max_ms: 100,
            })
        );
        assert_eq!(distribution(&[]), None);
    }
}
