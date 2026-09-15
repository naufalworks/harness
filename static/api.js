'use strict';
// P12-T03: the one place the frontend knows the HTTP contract.
//
// Before this file, every caller in `app.js` re-derived meaning from two things the server never
// promised to keep stable: the numeric status, and the human sentence in `error`. A refusal was
// recognised by `error.status === 409`, an admission failure by a hand-written list of status
// numbers. That works only as long as no handler ever answers a different status for the same
// situation, which is exactly the kind of change the server is free to make.
//
// The server now sends `{error, code, retryable}` on every JSON failure, so the client can branch
// on the code and let the sentence stay what it always was: text to show the reader, verbatim,
// never parsed.
//
// This is a classic script, loaded before `app.js` and sharing its global scope. It is deliberately
// not a module: `index.html` fingerprints each asset with `?v=<commit>` and the server serves both
// from memory, so a module graph would buy nothing and cost a second round trip before first paint.

// The codes `src/api/error.rs` can produce, including the `error` fallback for a status with no
// specific code. `tests/test_api_schema.py` compares this list against that function in both
// directions, so a code cannot be added, renamed or removed on the server without this file
// changing in the same commit. Recognising a code is not the same as handling it; the table exists
// so the check has something to compare, and so a typo in a branch below fails the gate.
const API_ERROR_CODES = Object.freeze([
  'invalid_request',
  'unauthorized',
  'forbidden',
  'not_found',
  'conflict',
  'gone',
  'payload_too_large',
  'unsupported_media_type',
  'unprocessable_body',
  'rate_limited',
  'internal_error',
  'not_implemented',
  'upstream_failed',
  'unavailable',
  'upstream_timeout',
  'error',
]);

// The recorded states a request can be in, mirroring `recording::RequestState`. Same two-way check
// as the codes above, and the labels are checked to cover every state: an unlabelled state used to
// render as a bare identifier like `interrupted` in the receipt panel.
const REQUEST_STATES = Object.freeze(['captured', 'generating', 'complete', 'failed', 'interrupted']);
const REQUEST_STATE_LABELS = Object.freeze({
  captured: 'Sent · waiting for answer',
  generating: 'Thinking…',
  complete: 'Done',
  failed: 'Saved · answer failed',
  interrupted: 'Saved · answer interrupted',
});
// `complete` is terminal but must never be offered for retry: that answer is already paid for.
const RETRYABLE_REQUEST_STATES = Object.freeze(['failed', 'interrupted']);

// Codes that mean the message was definitely not admitted, so the pending identity can be cleared
// and the draft handed back. Anything else - 5xx, a network drop, an unparsable body - may have
// happened *after* the server committed the request, so the identity is kept and the user is told
// to check rather than resend. This list replaces the old status-number list one for one.
const NOT_ADMITTED_CODES = Object.freeze([
  'invalid_request',
  'unauthorized',
  'forbidden',
  'payload_too_large',
  'unprocessable_body',
  'unavailable',
]);

class ApiError extends Error {
  constructor(message, { status, code, retryable }) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    // `null` means the server did not tell us, which is different from any particular code. A
    // caller must not read absence as a match, so every branch here tests an explicit value.
    this.code = code;
    this.retryable = retryable;
  }
  is(...codes) { return typeof this.code === 'string' && codes.includes(this.code); }
  get notAdmitted() { return this.is(...NOT_ADMITTED_CODES); }
}

// Network loss is a UI state, never a retry instruction. The event carries no request body or
// token; app.js uses it to show reconnecting without replaying an ambiguous submission.
function signalApiConnection(state) {
  if (typeof window !== 'undefined') {
    window.dispatchEvent(new CustomEvent('harness:connection', { detail: { state } }));
  }
}

// Read the envelope without trusting it. A failure body can be missing, truncated, or not JSON at
// all - an intermediary can answer for the server - so each field is taken only when it has the
// documented type, and the sentence falls back to something that still names the status.
function apiError(status, payload) {
  const body = payload && typeof payload === 'object' ? payload : {};
  const message = typeof body.error === 'string' && body.error ? body.error : `Request failed (${status})`;
  return new ApiError(message, {
    status,
    code: typeof body.code === 'string' ? body.code : null,
    retryable: typeof body.retryable === 'boolean' ? body.retryable : null,
  });
}

async function apiReadBody(response) {
  return response.json().catch(() => ({ error: `Unexpected response (${response.status})` }));
}

// One request path for every authenticated JSON route: GET when there is no body, POST when there
// is, bearer auth in the header so no token reaches a URL or a log, and a hard timeout so a hung
// connection cannot leave the composer disabled forever.
async function apiRequest(path, { token, body, method, timeoutMs = 20000 } = {}) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetch(path, {
      method: method || (body === undefined ? 'GET' : 'POST'),
      signal: controller.signal,
      headers: {
        'Authorization': `Bearer ${token}`,
        ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const payload = await apiReadBody(response);
    if (!response.ok) throw apiError(response.status, payload);
    signalApiConnection('reachable');
    return payload;
  } catch (error) {
    if (!(error instanceof ApiError)) signalApiConnection(error?.name === 'AbortError' ? 'degraded' : 'offline');
    throw error;
  } finally { clearTimeout(timer); }
}

// The master token is exchanged for a short-lived session token. A 2xx without a usable token is
// treated as a failure rather than a session, because storing a non-string here would send
// `Bearer undefined` on every later call and read as an auth problem far from its cause.
async function apiExchangeSession(masterToken, { timeoutMs = 20000 } = {}) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetch('/auth/session', {
      method: 'POST',
      signal: controller.signal,
      headers: { 'Authorization': `Bearer ${masterToken}` },
    });
    const payload = await apiReadBody(response);
    if (!response.ok || typeof payload.session_token !== 'string') throw apiError(response.status, payload);
    return payload.session_token;
  } finally { clearTimeout(timer); }
}
