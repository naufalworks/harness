//! Activity and generation feeds: polled JSON reads and their SSE live streams.
//!
//! Moved verbatim from `main.rs` by P12-T01; behaviour is unchanged.
use crate::api::error::{db_error, invalid, ApiResult};
use crate::Harness;
use axum::{
    extract::{Query, State},
    http::header,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivityQuery {
    session_id: String,
    #[serde(default)]
    after_seq: Option<i64>,
}
pub(crate) async fn activity(
    State(h): State<Harness>,
    Query(q): Query<ActivityQuery>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&q.session_id).map_err(|_| invalid("Invalid session identifier"))?;
    let after = q.after_seq.unwrap_or(0);
    if after < 0 {
        return Err(invalid("Activity cursor cannot be negative"));
    }
    Ok(Json(
        h.store
            .activity_since(q.session_id, after)
            .await
            .map_err(db_error)?,
    ))
}

// P2-T01: the same feed as a live stream. Every frame is a row `activity_events` already holds
// and the DB sequence is the only cursor, so the socket carries no state worth losing: a client
// that reconnects with the last `id` it saw is replayed from the row after it, exactly once.
// The token stays in the `Authorization` header (the client uses `fetch`, never `EventSource`,
// which cannot send one), and `authenticate` releases its concurrency permit as soon as the
// response head is returned, so an open stream never occupies one of the 8 API slots.
pub(crate) const STREAM_HEARTBEAT: Duration = Duration::from_secs(15);
const STREAM_BATCH: usize = 200; // `agentic_sql::EVENTS_AFTER` LIMIT
const STREAM_READ_FAILURES: u32 = 25; // ~5 s of failed reads, then close
/// The stream's only clock: an idle turn still says something every 15 s, so a client cannot
/// read a dead socket as a quiet agent. A sent event resets it; the comment is not an event.
pub(crate) fn heartbeat_due(quiet: Duration) -> bool {
    quiet >= STREAM_HEARTBEAT
}
/// One SSE frame per recorded row, `id` first so a reconnect can resume from it.
pub(crate) fn activity_frame(event: &Value) -> Option<String> {
    let seq = event["seq"].as_i64()?;
    Some(format!(
        "id: {seq}\nevent: {}\ndata: {event}\n\n",
        event["kind"].as_str().unwrap_or("activity")
    ))
}
struct Frames(tokio::sync::mpsc::Receiver<String>);
impl futures_core::Stream for Frames {
    type Item = std::result::Result<String, std::convert::Infallible>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.0.poll_recv(cx).map(|frame| frame.map(Ok))
    }
}
pub(crate) async fn activity_stream(
    State(h): State<Harness>,
    Query(q): Query<ActivityQuery>,
) -> ApiResult<Response> {
    Uuid::parse_str(&q.session_id).map_err(|_| invalid("Invalid session identifier"))?;
    let after = q.after_seq.unwrap_or(0);
    if after < 0 {
        return Err(invalid("Activity cursor cannot be negative"));
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(64);
    let mut rx_commit = h.store.commit_notify.subscribe();
    tokio::spawn(async move {
        let (mut cursor, mut quiet, mut failures) = (after, tokio::time::Instant::now(), 0u32);
        // A closed channel is the client hanging up; stop reading the database for a tab that left.
        while !tx.is_closed() {
            match h.store.activity_since(q.session_id.clone(), cursor).await {
                // Contention on the database queue is transient and must not look like "turn over":
                // hold the cursor, try again, and give up only after the failures stop being a blip.
                Err(_) => {
                    failures += 1;
                    if failures >= STREAM_READ_FAILURES {
                        return;
                    }
                }
                Ok(batch) => {
                    failures = 0;
                    let events = batch["events"].as_array().cloned().unwrap_or_default();
                    for event in &events {
                        let Some(frame) = activity_frame(event) else {
                            continue;
                        };
                        if tx.send(frame).await.is_err() {
                            return;
                        }
                        cursor = event["seq"].as_i64().unwrap_or(cursor);
                        quiet = tokio::time::Instant::now();
                    }
                    // A full batch means more rows are already committed; drain before sleeping.
                    if events.len() >= STREAM_BATCH {
                        continue;
                    }
                }
            }
            if heartbeat_due(quiet.elapsed()) {
                if tx.send(": heartbeat\n\n".into()).await.is_err() {
                    return;
                }
                quiet = tokio::time::Instant::now();
            }
            let until_heartbeat = STREAM_HEARTBEAT.saturating_sub(quiet.elapsed());
            let _ = tokio::time::timeout(until_heartbeat, rx_commit.recv()).await;
        }
    });
    Ok((
        [(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")],
        axum::body::Body::from_stream(Frames(rx)),
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GenerationQuery {
    session_id: String,
    #[serde(default)]
    after_seq: Option<i64>,
}

pub(crate) async fn generation(
    State(h): State<Harness>,
    Query(q): Query<GenerationQuery>,
) -> ApiResult<Json<Value>> {
    Uuid::parse_str(&q.session_id).map_err(|_| invalid("Invalid session identifier"))?;
    let after = q.after_seq.unwrap_or(0);
    if after < 0 {
        return Err(invalid("Generation cursor cannot be negative"));
    }
    Ok(Json(
        h.store
            .generation_since(q.session_id, after)
            .await
            .map_err(db_error)?,
    ))
}

fn generation_frame(event: &Value) -> Option<String> {
    let seq = event["seq"].as_i64()?;
    Some(format!("id: {seq}\nevent: generation\ndata: {event}\n\n"))
}

pub(crate) async fn generation_stream(
    State(h): State<Harness>,
    Query(q): Query<GenerationQuery>,
) -> ApiResult<Response> {
    Uuid::parse_str(&q.session_id).map_err(|_| invalid("Invalid session identifier"))?;
    let after = q.after_seq.unwrap_or(0);
    if after < 0 {
        return Err(invalid("Generation cursor cannot be negative"));
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(64);
    let mut rx_commit = h.store.commit_notify.subscribe();
    tokio::spawn(async move {
        let (mut cursor, mut quiet, mut failures) = (after, tokio::time::Instant::now(), 0u32);
        while !tx.is_closed() {
            match h.store.generation_since(q.session_id.clone(), cursor).await {
                Err(_) => {
                    failures += 1;
                    if failures >= STREAM_READ_FAILURES {
                        return;
                    }
                }
                Ok(batch) => {
                    failures = 0;
                    let events = batch["events"].as_array().cloned().unwrap_or_default();
                    for event in &events {
                        let Some(frame) = generation_frame(event) else {
                            continue;
                        };
                        if tx.send(frame).await.is_err() {
                            return;
                        }
                        cursor = event["seq"].as_i64().unwrap_or(cursor);
                        quiet = tokio::time::Instant::now();
                    }
                    if events.len() >= STREAM_BATCH {
                        continue;
                    }
                }
            }
            if heartbeat_due(quiet.elapsed()) {
                if tx.send(": heartbeat\n\n".into()).await.is_err() {
                    return;
                }
                quiet = tokio::time::Instant::now();
            }
            let until_heartbeat = STREAM_HEARTBEAT.saturating_sub(quiet.elapsed());
            let _ = tokio::time::timeout(until_heartbeat, rx_commit.recv()).await;
        }
    });
    Ok((
        [(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")],
        axum::body::Body::from_stream(Frames(rx)),
    )
        .into_response())
}
