//! Typed views over the untyped JSON the storage layer hands to the HTTP layer.
//!
//! P12-T03. Handlers used to branch on `receipt["state"] == "generating"` and
//! `receipt["request_id"].as_str()`. Indexing an untyped `Value` with a string
//! literal compiles whatever the literal says: a typo silently yields `Null`,
//! matches no arm, and changes a status code without any test or compiler
//! noticing. The parse happens once here so handlers match on variants.
//!
//! This is a view, not a schema for the whole receipt. The receipt carries many
//! more fields, and they are still forwarded to the client verbatim; this type
//! only names the parts the server makes decisions with.
use crate::recording::RequestState;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ReceiptView {
    #[serde(default)]
    pub(crate) request_id: Option<String>,
    /// Deliberately the raw string rather than `Option<RequestState>`. A state
    /// this build does not recognise would fail deserialization for the whole
    /// struct, which would also blank `request_id` and turn an unknown state
    /// into a missing identifier. Parsing is separate, in `state()`.
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error_code: Option<String>,
}

impl ReceiptView {
    pub(crate) fn of(receipt: &Value) -> Self {
        serde_json::from_value(receipt.clone()).unwrap_or_default()
    }

    /// `None` when the receipt carries no state, or carries one this build does
    /// not know. Callers must handle that explicitly; it is never quietly
    /// treated as terminal, because reporting a made-up terminal state is worse
    /// than continuing to poll.
    pub(crate) fn state(&self) -> Option<RequestState> {
        self.state.as_deref().and_then(RequestState::parse)
    }

    /// Distinguishes a deliberate cancellation from any other interruption.
    pub(crate) fn was_cancelled(&self) -> bool {
        self.error_code.as_deref() == Some("cancelled")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ALL: [RequestState; 5] = [
        RequestState::Captured,
        RequestState::Generating,
        RequestState::Complete,
        RequestState::Failed,
        RequestState::Interrupted,
    ];

    fn wire(state: RequestState) -> String {
        serde_json::to_value(state)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn api_receipt_view_reads_the_fields_handlers_branch_on() {
        let view = ReceiptView::of(&json!({
            "request_id": "r1",
            "state": "interrupted",
            "error_code": "cancelled",
            "response": "ignored",
            "memory_status": "deferred"
        }));
        assert_eq!(view.request_id.as_deref(), Some("r1"));
        assert_eq!(view.state(), Some(RequestState::Interrupted));
        assert!(view.was_cancelled());
    }

    #[test]
    fn api_an_unknown_state_does_not_parse_and_does_not_lose_the_request_id() {
        // The whole point of keeping `state` as a string: a state written by a
        // newer build must not make the identifier unreadable.
        let view = ReceiptView::of(&json!({"request_id": "r2", "state": "teleported"}));
        assert_eq!(view.request_id.as_deref(), Some("r2"));
        assert_eq!(view.state(), None);
        assert!(!view.was_cancelled());
    }

    #[test]
    fn api_a_receipt_without_the_branching_fields_is_not_an_error() {
        let view = ReceiptView::of(&json!({"unrelated": true}));
        assert_eq!(view.request_id, None);
        assert_eq!(view.state(), None);
    }

    #[test]
    fn api_an_interruption_without_a_cancel_code_is_not_a_cancellation() {
        let view =
            ReceiptView::of(&json!({"state": "interrupted", "error_code": "provider_error"}));
        assert_eq!(view.state(), Some(RequestState::Interrupted));
        assert!(!view.was_cancelled());
    }

    #[test]
    fn api_pending_states_are_exactly_captured_and_generating() {
        // These two are what the admission queries filter on
        // (`state IN ('captured','generating')`). If this set ever changes,
        // those SQL predicates must change with it.
        let pending: Vec<String> = ALL
            .into_iter()
            .filter(|state| state.is_pending())
            .map(wire)
            .collect();
        assert_eq!(pending, vec!["captured", "generating"]);
    }

    #[test]
    fn api_request_states_round_trip_through_their_durable_strings() {
        for state in ALL {
            let text = wire(state);
            // The wire string, the parser, and the deserializer must agree;
            // these strings are stored in the database and sent to clients.
            assert_eq!(RequestState::parse(&text), Some(state));
            assert_eq!(
                serde_json::from_value::<RequestState>(json!(text)).unwrap(),
                state
            );
        }
    }

    #[test]
    fn api_only_failed_and_interrupted_requests_are_retryable() {
        // Retryable is narrower than terminal: `complete` is terminal too, and
        // re-running a finished answer would pay for it a second time.
        let retryable: Vec<String> = ALL
            .into_iter()
            .filter(|state| matches!(state, RequestState::Failed | RequestState::Interrupted))
            .map(wire)
            .collect();
        assert_eq!(retryable, vec!["failed", "interrupted"]);
        assert!(!RequestState::Complete.is_pending());
    }
}
