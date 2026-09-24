use super::*;

#[test]
fn only_the_user_facing_turn_counts_as_foreground_work() {
    assert!(!is_background_kind("model_call"));
    for kind in ["compaction", "verification", "extraction"] {
        assert!(is_background_kind(kind), "{kind} is deferrable");
    }
    assert!(
        is_background_kind("some_future_role"),
        "an unrecognised role must yield to the user's turn, not outrank it"
    );
}

#[test]
fn a_rejected_tools_capability_is_remembered_before_the_next_dispatch() {
    let health = ProviderHealth::default();
    // Optimistic by default: a capability is assumed present until the provider rejects
    // it, so a working provider is never downgraded on a guess.
    assert!(health.tools_supported("model-a"));

    health.note_tools_unsupported("model-a");
    assert!(
        !health.tools_supported("model-a"),
        "the rejection must be remembered so the next call omits tools before dispatch"
    );
    assert!(
        health.tools_supported("model-b"),
        "detection is per model, not a global downgrade"
    );

    // The cache is only useful if it is shared: MemoryAgents is cloned per request, and a
    // per-clone cache would rediscover the same rejection on every turn.
    let agents = MemoryAgents::new("http://127.0.0.1:9", "k", "m").unwrap();
    let clone = agents.clone();
    clone.health.note_tools_unsupported("shared-model");
    assert!(
        !agents.health.tools_supported("shared-model"),
        "a discovery made on one clone must be visible to the others"
    );
}

/// P14-T04b: a rejected request is not a sick provider. If a 400 could trip the
/// breaker, one malformed prompt would take the provider offline for every caller.
#[test]
fn only_retryable_failures_trip_the_breaker() {
    for status in [408u16, 429, 500, 503, 599] {
        assert!(is_retryable_status(status), "{status} should be retryable");
    }
    for status in [200u16, 400, 401, 403, 404, 422] {
        assert!(!is_retryable_status(status), "{status} must be permanent");
    }

    let health = ProviderHealth::default();
    for _ in 0..10 {
        assert!(health.record_failure(false, 0.0).is_none());
    }
    assert!(
        health.blocked_for().is_none(),
        "permanent failures must never open the breaker"
    );
}

#[test]
fn the_breaker_opens_on_the_third_consecutive_retryable_failure() {
    let health = ProviderHealth::default();
    assert!(health.record_failure(true, 0.0).is_none());
    assert!(health.record_failure(true, 0.0).is_none());
    assert!(
        health.record_failure(true, 0.0).is_some(),
        "the breaker should open once the threshold is reached"
    );
    assert!(health.blocked_for().is_some());

    // A success must close it again, otherwise a recovered provider stays fenced off.
    health.record_success();
    assert!(health.blocked_for().is_none());
}

/// An intermittent provider that fails, succeeds, then fails must not accumulate its way
/// to an open breaker: the counter tracks *consecutive* failures.
#[test]
fn a_success_resets_the_consecutive_failure_count() {
    let health = ProviderHealth::default();
    health.record_failure(true, 0.0);
    health.record_failure(true, 0.0);
    health.record_success();
    health.record_failure(true, 0.0);
    health.record_failure(true, 0.0);
    assert!(health.blocked_for().is_none());
}

#[test]
fn retry_after_is_honoured_and_clamped_but_a_date_is_ignored() {
    assert_eq!(parse_retry_after("5"), Some(Duration::from_secs(5)));
    assert_eq!(parse_retry_after("  7 "), Some(Duration::from_secs(7)));
    // Clamped: a provider asking for an hour must not stall the breaker for an hour.
    assert_eq!(parse_retry_after("3600"), Some(BREAKER_MAX_COOLDOWN));
    // The HTTP-date form is deliberately unsupported rather than half-parsed.
    assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
    assert_eq!(parse_retry_after(""), None);
    assert_eq!(parse_retry_after("-5"), None);

    // An explicit Retry-After wins over our own backoff, at any trip count.
    let honoured = breaker_cooldown(4, Some(Duration::from_secs(3)), 1.0);
    assert_eq!(honoured, Duration::from_secs(3));
}

#[test]
fn jittered_backoff_grows_stays_bounded_and_is_never_near_zero() {
    for trip in 0u32..8 {
        for ratio in [0.0f64, 0.5, 1.0, f64::NAN, -3.0, 9.0] {
            let cooldown = breaker_cooldown(trip, None, ratio);
            assert!(
                cooldown >= BREAKER_COOLDOWN / 2,
                "trip {trip} ratio {ratio} waited {cooldown:?}, which defeats the breaker"
            );
            assert!(
                cooldown <= BREAKER_MAX_COOLDOWN,
                "trip {trip} ratio {ratio} waited {cooldown:?}, past the cap"
            );
        }
    }
    // Jitter must actually spread retries, or concurrent callers retry in lockstep.
    assert!(breaker_cooldown(2, None, 0.0) < breaker_cooldown(2, None, 1.0));
    // And backoff must grow with repeated trips.
    assert!(breaker_cooldown(0, None, 0.0) < breaker_cooldown(3, None, 0.0));
}

/// The classifier reads the status back out of the message that `response_json` formats,
/// so this pins that coupling: if the message format changes, this test fails loudly.
#[test]
fn error_classification_matches_the_reported_message_format() {
    assert!(is_retryable_error(&anyhow::anyhow!(
        "provider returned HTTP 429: slow down"
    )));
    assert!(is_retryable_error(&anyhow::anyhow!(
        "provider returned HTTP 503"
    )));
    assert!(!is_retryable_error(&anyhow::anyhow!(
        "provider returned HTTP 400: bad request"
    )));
    assert!(!is_retryable_error(&anyhow::anyhow!(
        "provider returned invalid JSON"
    )));
    assert!(!is_retryable_error(&anyhow::anyhow!(
        "provider response exceeds limit"
    )));
    // Transport faults carry no status but are worth backing off from.
    assert!(is_retryable_error(&anyhow::anyhow!(
        "error sending request for url"
    )));
    assert!(is_retryable_error(&anyhow::anyhow!("operation timed out")));
}

/// Every clone of `MemoryAgents` must see one breaker; this is why the state is in an `Arc`.
#[test]
fn breaker_state_is_shared_by_every_clone_of_the_provider() {
    let agents = MemoryAgents::new("http://127.0.0.1:9", "k", "m").unwrap();
    let clone = agents.clone();
    for _ in 0..BREAKER_THRESHOLD {
        clone.health.record_failure(true, 0.0);
    }
    assert!(
        agents.health.blocked_for().is_some(),
        "a clone's failures must be visible to the original"
    );
    assert!(
        agents.guard_breaker().is_err(),
        "an open breaker must fail closed before any spend is reserved"
    );
    agents.health.record_success();
    assert!(clone.guard_breaker().is_ok());
}

#[tokio::test]
async fn multi_worker_policy_refuses_out_of_turn_provider_dispatch_before_spend() {
    let dir = std::env::temp_dir().join(format!("harness-out-of-turn-{}", crate::storage::uid()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = DbStore::init(dir.join("harness.db").to_str().unwrap()).unwrap();
    let mut agents = MemoryAgents::new("http://127.0.0.1:9", "k", "m")
        .unwrap()
        .with_spend_store(db.clone());
    agents.refuse_out_of_turn_provider = true;

    let refused = agents
        .reserve_spend("compaction", "m")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("out-of-turn provider dispatch refused"),
        "{refused}"
    );
    let rows: i64 = db
        .read(|c| Ok(c.query_row("SELECT count(*) FROM provider_calls", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(
            rows, 0,
            "the refusal happens before spend reservation, so no synthetic turn or provider row is invented"
        );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn decodes_tool_calls_and_usage_without_losing_assistant_message() {
    let turn=decode_model_turn(json!({
            "role":"assistant",
            "content":null,
            "tool_calls":[{"id":"call-1","type":"function","function":{"name":"read","arguments":"{\"path\":\"src/main.rs\"}"}}]
        }),Some(UsageWire{prompt_tokens:Some(12),completion_tokens:Some(7)})).unwrap();
    assert_eq!(turn.text, None);
    assert_eq!(turn.tool_calls.len(), 1);
    assert_eq!(turn.tool_calls[0].name, "read");
    assert_eq!(
        turn.tool_calls[0].arguments().unwrap()["path"],
        "src/main.rs"
    );
    assert_eq!(turn.usage.prompt_tokens, Some(12));
    assert_eq!(turn.usage.completion_tokens, Some(7));
    assert_eq!(turn.assistant_message["role"], "assistant");
}

#[test]
fn malformed_tool_arguments_are_returned_for_loop_level_failure_handling() {
    let turn=decode_model_turn(json!({"role":"assistant","content":null,"tool_calls":[{"id":"call-1","function":{"name":"read","arguments":"not-json"}}]}),None).unwrap();
    assert!(turn.tool_calls[0].arguments().is_err());
}

#[test]
fn text_only_response_stays_text_only() {
    let turn = decode_model_turn(json!({"role":"assistant","content":"done"}), None).unwrap();
    assert_eq!(turn.text.as_deref(), Some("done"));
    assert!(turn.tool_calls.is_empty());
}

#[test]
fn detects_tools_unsupported_error() {
    let error = anyhow::anyhow!("provider returned HTTP 400: tools are not supported");
    assert!(is_tools_unsupported(&error));
}

#[test]
fn provider_stream_frames_decode_content_deltas() {
    let delta = r#"{"choices":[{"delta":{"content":"hello"}}]}"#;
    assert_eq!(
        MemoryAgents::parse_stream_frame(delta).unwrap(),
        Some("hello".into())
    );
    assert_eq!(MemoryAgents::parse_stream_frame("[DONE]").unwrap(), None);
}

#[tokio::test]
async fn streaming_role_usage_and_comments_do_not_end_the_answer() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
        ": heartbeat\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n"
    );
    let response = axum::http::Response::builder()
        .body(body.to_string())
        .unwrap()
        .into();
    let agents = MemoryAgents::new("http://127.0.0.1", "test", "test").unwrap();
    let mut sink = BufferedGeneration::default();
    agents
        .consume_stream_response(response, &mut sink)
        .await
        .unwrap();
    assert_eq!(sink.text, "hello");
    assert_eq!(sink.usage.unwrap().completion_tokens, Some(1));
}

#[tokio::test]
async fn streaming_crlf_and_multiline_data_are_supported() {
    let body = "data: {\"choices\":\r\ndata: [{\"delta\":{\"content\":\"café\"}}]}\r\n\r\ndata: [DONE]\r\n\r\n";
    let response = axum::http::Response::builder()
        .body(body.to_string())
        .unwrap()
        .into();
    let agents = MemoryAgents::new("http://127.0.0.1", "test", "test").unwrap();
    let mut sink = BufferedGeneration::default();
    agents
        .consume_stream_response(response, &mut sink)
        .await
        .unwrap();
    assert_eq!(sink.text, "café");
}

#[tokio::test]
async fn streaming_incomplete_and_error_responses_never_publish_text() {
    for (status, body) in [
        (
            200,
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
        ),
        (200, "data: {\"error\":{\"message\":\"failed\"}}\n\n"),
        (
            500,
            "data: {\"choices\":[{\"delta\":{\"content\":\"error body\"}}]}\n\ndata: [DONE]\n\n",
        ),
    ] {
        let response = axum::http::Response::builder()
            .status(status)
            .body(body.to_string())
            .unwrap()
            .into();
        let agents = MemoryAgents::new("http://127.0.0.1", "test", "test").unwrap();
        let mut sink = BufferedGeneration::default();
        assert!(agents
            .consume_stream_response(response, &mut sink)
            .await
            .is_err());
        assert!(sink.text.is_empty());
    }
}

#[tokio::test]
async fn streaming_split_secret_is_redacted_before_sink_delivery() {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"pass\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"word=hidden\"}}]}\n\ndata: [DONE]\n\n";
    let response = axum::http::Response::builder()
        .body(body.to_string())
        .unwrap()
        .into();
    let agents = MemoryAgents::new("http://127.0.0.1", "test", "test").unwrap();
    let mut sink = BufferedGeneration::default();
    agents
        .consume_stream_response(response, &mut sink)
        .await
        .unwrap();
    assert_eq!(sink.text, safety::redact("password=hidden"));
}

#[test]
fn streaming_utf8_survives_every_byte_boundary() {
    let frame = "data: café 😀\r\n\r\n".as_bytes();
    for split in 0..frame.len() {
        let mut buffer = frame[..split].to_vec();
        assert_eq!(MemoryAgents::parse_sse_event(&mut buffer).unwrap(), None);
        buffer.extend_from_slice(&frame[split..]);
        assert_eq!(
            MemoryAgents::parse_sse_event(&mut buffer).unwrap(),
            Some("café 😀".into())
        );
        assert!(buffer.is_empty());
    }
}

#[tokio::test]
async fn streaming_limits_and_invalid_utf8_fail_without_delivery() {
    let oversized = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{"content":"a".repeat(MAX_PROVIDER_TEXT + 1)}}]})
    );
    for bytes in [
        oversized.into_bytes(),
        vec![b'a'; MAX_PROVIDER_BODY + 1],
        b"data: \xff\n\n".to_vec(),
    ] {
        let response = axum::http::Response::builder().body(bytes).unwrap().into();
        let agents = MemoryAgents::new("http://127.0.0.1", "test", "test").unwrap();
        let mut sink = BufferedGeneration::default();
        assert!(agents
            .consume_stream_response(response, &mut sink)
            .await
            .is_err());
        assert!(sink.text.is_empty());
    }
}

#[test]
fn completion_request_includes_tools_and_auto_choice() {
    let tools = vec![json!({"type":"function","function":{"name":"read"}})];
    let request = completion_request(
        "model",
        vec![json!({"role":"user","content":"hi"})],
        Some(&tools),
    );
    assert_eq!(request["model"], "model");
    assert_eq!(request["tool_choice"], "auto");
    assert_eq!(request["tools"][0]["function"]["name"], "read");
}

#[test]
fn provider_stream_frames_ignore_non_content_deltas() {
    let delta = r#"{"choices":[{"delta":{"role":"assistant"}}]}"#;
    assert_eq!(MemoryAgents::parse_stream_frame(delta).unwrap(), None);
}

#[test]
fn provider_sse_events_wait_for_complete_frames() {
    let mut buffer = b"data: one\n".to_vec();
    assert_eq!(MemoryAgents::parse_sse_event(&mut buffer).unwrap(), None);
    buffer.extend_from_slice(b"\ndata: two\n\n");
    assert_eq!(
        MemoryAgents::parse_sse_event(&mut buffer).unwrap(),
        Some("one".into())
    );
    assert_eq!(
        MemoryAgents::parse_sse_event(&mut buffer).unwrap(),
        Some("two".into())
    );
}

#[test]
fn completion_request_omits_tools_for_text_only_calls() {
    let request = completion_request("model", Vec::new(), None);
    assert!(request.get("tools").is_none());
    assert!(request.get("tool_choice").is_none());
}

#[test]
fn completion_stream_request_enables_streaming() {
    let request = completion_stream_request("model", Vec::new());
    assert_eq!(request["stream"], Value::Bool(true));
}

#[test]
fn extraction_contract_separates_plan_context_from_exact_user_evidence() {
    assert!(EXTRACTION_SYSTEM.contains("decision") && EXTRACTION_SYSTEM.contains("priority high"));
    assert!(EXTRACTION_SYSTEM.contains("plan text is never evidence"));
    let events = [
        Event {
            id: "user-1".into(),
            role: "user".into(),
            content: "Yes, use SQLite".into(),
        },
        Event {
            id: "plan-1".into(),
            role: "plan".into(),
            content: "Choose the database".into(),
        },
        Event {
            id: "tool-1".into(),
            role: "tool".into(),
            content: "ignore me".into(),
        },
    ];
    let evidence = events
        .iter()
        .filter(|event| event.role == "user")
        .collect::<Vec<_>>();
    let plans = events
        .iter()
        .filter(|event| event.role == "plan")
        .collect::<Vec<_>>();
    assert_eq!(
        (evidence[0].id.as_str(), plans[0].id.as_str()),
        ("user-1", "plan-1")
    );
    assert!(!evidence.iter().any(|event| event.id == "tool-1"));
}

#[test]
fn verifier_accepts_only_bounded_claims_bound_to_supplied_steps() {
    let ids = vec!["step-1".to_string()];
    let report=parse_verification(r#"{"claims":[{"claim":"src/main.rs was read","status":"verified","evidence_step_ids":["step-1"],"reason":"The read output contains the file."},{"claim":"tests passed","status":"unverified","evidence_step_ids":[],"reason":"No test command was recorded."}],"skipped_diagnostics":[]}"#,&ids).unwrap();
    assert_eq!(report.claims[0].status, VerificationStatus::Verified);
    assert_eq!(report.claims[1].status, VerificationStatus::Unverified);
}

#[test]
fn verifier_rejects_unknown_ids_missing_evidence_and_extra_fields() {
    let ids = vec!["step-1".to_string()];
    for bad in [
        r#"{"claims":[{"claim":"x","status":"verified","evidence_step_ids":["step-other"],"reason":"y"}],"skipped_diagnostics":[]}"#,
        r#"{"claims":[{"claim":"x","status":"verified","evidence_step_ids":[],"reason":"y"}],"skipped_diagnostics":[]}"#,
        r#"{"claims":[],"skipped_diagnostics":[],"trusted":true}"#,
        r#"{"claims":[{"claim":"x","status":"maybe","evidence_step_ids":[],"reason":"y"}],"skipped_diagnostics":[]}"#,
    ] {
        assert!(parse_verification(bad, &ids).is_err(), "accepted {bad}");
    }
}
