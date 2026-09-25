'use strict';
const $ = id => document.getElementById(id);
const EXPECTED_SERVER_COMMIT = '__HARNESS_BUILD_COMMIT__';
let token = '';
let scope = sessionStorage.getItem('harness_scope') || 'global';
let scopeConfig = null;
let session = sessionStorage.getItem('harness_session') || crypto.randomUUID();
let busy = false, epoch = 0, historyCursor = null, sessionsCursor = null;
let pending = null, pendingPrompt = null;
try { pending = JSON.parse(sessionStorage.getItem('harness_pending') || 'null'); } catch { sessionStorage.removeItem('harness_pending'); }
// PENDING_FIELDS is exactly what /chat/submit accepts, so it is also all that goes on the wire: a
// draft identity stored by another build can carry keys the server rejects, and posting those back
// made every send fail with a 422 the UI could not parse. generation_cursor is kept because P7-T03
// resumes the generation stream after a reload, but it stays in this tab and is never sent.
const PENDING_FIELDS = ['request_id', 'session_id', 'scope'];
if (pending && typeof pending === 'object') {
  const kept = Object.fromEntries(PENDING_FIELDS.filter(key => typeof pending[key] === 'string').map(key => [key, pending[key]]));
  if (Number.isFinite(pending.generation_cursor)) kept.generation_cursor = pending.generation_cursor;
  pending = kept; sessionStorage.setItem('harness_pending', JSON.stringify(pending));
}
if (pending && (typeof pending.request_id !== 'string' || pending.session_id !== session || pending.scope !== scope)) { pending = null; sessionStorage.removeItem('harness_pending'); }
$('scope').value = scope;
function notice(text, error = false) { $('notice').textContent = text; $('notice').classList.toggle('error', error); }
function setConnectionState(state, label) {
  const badge = $('connection');
  badge.dataset.state = state;
  badge.textContent = label;
}
window.addEventListener('harness:connection', event => {
  const state = event.detail?.state;
  if (state === 'offline') {
    setConnectionState('offline', 'Offline · retrying');
    notice('Connection lost. Requests are not retried automatically; reconnecting…', true);
  } else if (state === 'degraded') {
    setConnectionState('degraded', 'Connection delayed');
    notice('The server did not respond in time. Check status before retrying.', true);
  }
});
window.addEventListener('offline', () => {
  if (token) setConnectionState('offline', 'Offline · retrying');
});
window.addEventListener('online', () => {
  if (!token) return;
  setConnectionState('reconnecting', 'Reconnecting…');
  notice('Connection restored. Checking status without resending anything.');
  refreshStatus().catch(() => {});
});
function captureLabel(text) { $('capturestatus').textContent = text; }
function rememberPending(value) {
  pending = value;
  if (value) sessionStorage.setItem('harness_pending', JSON.stringify(value));
  else { sessionStorage.removeItem('harness_pending'); pendingPrompt = null; }
  $('checkrecording').hidden = !value; $('leavepending').hidden = !value; $('send').hidden = !!value; $('cancelrequest').hidden = !value;
}
// P12-T03: both wrappers now delegate to `api.js`, which owns the request shape and the error
// envelope. `error.status` and `error.message` keep their old meaning for the many callers that
// only display the sentence; `error.code` is what new branches test. A thrown value is only an
// `ApiError` when the server actually answered - an aborted or dropped request is not - so every
// branch below checks the type before reading a code, and an unrecognised failure keeps the
// cautious path it had before.
async function api(path, body) { return apiRequest(path, { token, body }); }
async function apiPatch(path, body) { return apiRequest(path, { token, body, method: 'PATCH' }); }
async function apiDelete(path) { return apiRequest(path, { token, method: 'DELETE' }); }
async function exchangeBrowserSession(masterToken) { return apiExchangeSession(masterToken); }
function node(tag, text, cls) { const element = document.createElement(tag); if (text !== undefined) element.textContent = text; if (cls) element.className = cls; return element; }
function persistSession() { sessionStorage.setItem('harness_scope', scope); sessionStorage.setItem('harness_session', session); }
// Owned by `api.js` so the gate can check that every recorded state has a label. A state with no
// label used to render as a bare identifier in the receipt panel.
const stateLabels = REQUEST_STATE_LABELS;
const eventLabels = { captured: 'Message saved locally', generation_started: 'Answer started', context_saved: 'Context receipt saved', answer_saved: 'Answer saved locally', generation_failed: 'Answer did not complete', interrupted: 'Server restarted; no automatic resend', extraction_queued: 'Memory review queued', provider_spend_refused: 'Provider call refused by spend limit', cancel_requested: 'Cancellation requested', turn_cancelled: 'Turn cancelled at a recorded boundary', retry_created: 'Retry started from a safe boundary' };
async function showReceipt(id, content, button) {
  const myEpoch = epoch; button.disabled = true;
  try {
    const data = await api(`/chat/requests/${encodeURIComponent(id)}/context`);
    if (!token || myEpoch !== epoch) return;
    content.replaceChildren(node('p', stateLabels[data.state] || data.state, 'receipt-state'));
    const memory = {waiting_for_turn:'Waiting for the answer',deferred:'Waiting for queue space — the chat is already saved',pending:'Queued for extraction',running:'Extracting suggestions',done:'Extraction finished; suggestions still require approval',failed:'Extraction failed — the chat is still saved'};
    content.append(node('p', `Memory: ${memory[data.memory_status] || data.memory_status}`, 'muted'));
    if (data.redacted) content.append(node('p', 'Sensitive-looking input was filtered. This is a sanitized record, not an exact original.', 'muted'));
    const timeline = node('ol', undefined, 'receipt-timeline');
    for (const event of data.events || []) {
      const item = node('li', eventLabels[event.kind] || event.kind);
      const date = new Date(event.at); item.append(node('span', Number.isNaN(date.getTime()) ? '' : date.toLocaleTimeString([], {hour:'2-digit',minute:'2-digit'}), 'muted')); timeline.append(item);
    }
    content.append(timeline);
    const inspect = node('button', 'Inspect incident', 'secondary'); inspect.type = 'button';
    inspect.addEventListener('click', async () => { inspect.disabled = true; try { await refreshAgentTurn(data); $('rail').hidden = false; } catch (error) { notice(error.message, true); } finally { inspect.disabled = false; } });
    content.append(inspect);
    if (data.context) {
      content.append(node('h3', 'Context used for this turn'), node('p', 'What Harness prepared for the model—not proof of delivery or an explanation of the model’s reasoning.', 'muted'));
      for (const m of data.context.memories || []) { const card = node('div', undefined, 'receipt-memory'); card.append(node('strong', `${m.key} · revision ${m.revision}`), node('p', m.value), node('p', m.evidence?.quote || 'Original evidence unavailable', 'muted')); content.append(card); }
      if (!data.context.memories?.length) content.append(node('p', 'No active memories were supplied for this turn.', 'muted'));
      const detail = node('details'); detail.append(node('summary', 'Inspect prepared model messages'), node('pre', JSON.stringify(data.context.provider_messages, null, 2))); content.append(detail);
    } else content.append(node('p', 'No model context was recorded yet.', 'muted'));
    button.textContent = 'Refresh receipt';
  } catch (error) { if (token && myEpoch === epoch) content.replaceChildren(node('p', `Could not load receipt. ${error.message}`, 'muted')); }
  finally { button.disabled = false; }
}
// P11-T05: a long-lived tab must not grow without bound. The visible log keeps at most
// MAX_RENDERED_MESSAGES nodes and the sidebar at most MAX_RENDERED_SESSIONS entries. Nothing is
// lost by trimming: the server remains the source of truth and "Load older messages" re-fetches
// anything dropped, so this bounds DOM size and layout cost, never history.
const MAX_RENDERED_MESSAGES = 300, MAX_RENDERED_SESSIONS = 200;
function boundLog(prepended = false) {
  const log = $('log');
  let extra = log.childNodes.length - MAX_RENDERED_MESSAGES;
  if (extra <= 0) return;
  // Trim the end the reader is moving away from: paging upwards drops the far bottom,
  // newer arrivals drop the oldest nodes at the top.
  while (extra-- > 0 && log.childNodes.length) log.removeChild(prepended ? log.lastChild : log.firstChild);
  if (!prepended && historyCursor) $('oldermessages').hidden = false;
}
function message(m, target = $('log')) {
  const box = node('article', undefined, `message ${m.role === 'user' ? 'user' : 'assistant'}`);
  box.append(node('strong', m.role === 'user' ? 'You' : 'Harness'), node('span', m.content));
  if (m.role === 'user') {
    box.append(node('span', stateLabels[m.generation_state] || (m.status === 'complete' ? 'Saved' : 'Saved · no answer'), 'muted'));
    if (m.request_id) {
      const detail = node('details', undefined, 'receipt'); const content = node('div', undefined, 'receipt-content'); const button = node('button', 'Load receipt', 'secondary'); button.type = 'button';
      detail.append(node('summary', 'Message details'), content, button);
      button.addEventListener('click', () => showReceipt(m.request_id, content, button));
      detail.addEventListener('toggle', () => { if (detail.open && !content.childNodes.length && !button.disabled) showReceipt(m.request_id, content, button); });
      box.append(detail);
      const tray = node('section', undefined, 'suggestion-tray'); tray.dataset.requestId = m.request_id; tray.hidden = true; box.append(tray);
    }
  } else if (m.request_id) {
    box.append(node('span', 'Done · saved response', 'muted generation-state'));
  }
  target.append(box);
  if (m.role === 'user' && ['failed','interrupted'].includes(m.generation_state)) {
    const terminal = node('article', undefined, `message assistant generation-message ${m.generation_state}`);
    terminal.dataset.requestId = m.request_id || '';
    terminal.dataset.state = m.generation_state;
    terminal.append(
      node('strong', 'Harness'),
      node('span', m.generation_state === 'interrupted'
        ? 'The server restarted before an answer was saved. Nothing was resent.'
        : 'The provider failed before an answer was saved.'),
      node('span', m.generation_state === 'interrupted' ? 'Interrupted' : 'Failed', 'muted generation-state'),
    );
    if (m.request_id) {
      const retry = node('button', 'Retry from safe boundary', 'secondary');
      retry.type = 'button';
      retry.title = 'Starts a new turn from the last recorded non-mutating boundary. A completed side effect is never replayed.';
      retry.addEventListener('click', () => retryFromBoundary(m.request_id, retry));
      terminal.append(retry);
    }
    target.append(terminal);
  }
}
async function refreshStatus() {
  const myEpoch = epoch; const [health, data] = await Promise.all([api('/health'), api('/memory/status')]);
  if (!token || myEpoch !== epoch) return;
  if (!health.ready) throw new Error('Harness is not ready');
  if (health.commit !== EXPECTED_SERVER_COMMIT) throw new Error(`Stale UI detected (${EXPECTED_SERVER_COMMIT.slice(0, 7)} != ${String(health.commit).slice(0, 7)}); reload this page`);
  $('pending').textContent = String(data.pending_confirmations);
  setConnectionState('ready', `Ready · ${health.commit.slice(0, 7)}`);
  $('stats').textContent = `${data.active_memories} memories remembered${data.queued_jobs ? ` · ${data.queued_jobs} jobs queued` : ''}${data.failed_jobs ? ` · ${data.failed_jobs} failed` : ''} · build ${health.commit.slice(0, 7)}`;
}
async function loadHistory(older = false) {
  const myEpoch = epoch, mySession = session;
  const suffix = older && historyCursor ? `?before_seq=${historyCursor}` : '';
  const data = await api(`/sessions/${encodeURIComponent(session)}/messages${suffix}`);
  if (!token || myEpoch !== epoch || mySession !== session) return;
  if (data.scope && data.scope !== scope) { notice('This conversation belongs to another scope. Reopen it from History.', true); return; }
  const fragment = document.createDocumentFragment();
  for (const m of data.messages) message(m, fragment);
  // P2-T02: the scroll container is #chatscroll now; #log is a plain flow child of it.
  if (older) { const top = $('chatscroll').scrollHeight; $('log').prepend(fragment); $('chatscroll').scrollTop += $('chatscroll').scrollHeight - top; }
  else { $('log').replaceChildren(fragment); $('chatscroll').scrollTop = $('chatscroll').scrollHeight; }
  if (!data.messages.length && !older) $('log').append(node('p', 'Say hi to get started. Harness keeps up with the conversation and remembers what matters.', 'empty'));
  historyCursor = data.next_before_seq; $('oldermessages').hidden = !data.has_more;
  // Bound the rendered log after the cursor is known, so trimming can re-expose "Load older".
  boundLog(older);
  if (!older) {
    const last = [...data.messages].reverse().find(m => m.role === 'user');
    captureLabel(last ? stateLabels[last.generation_state] || '' : '');
    if (!pending && last?.request_id && ['captured','generating'].includes(last.generation_state)) rememberPending({request_id:last.request_id,session_id:session,scope});
  }
  await refreshInlineSuggestions().catch(() => {});
}
async function loadSessions(older = false) {
  const myEpoch = epoch;
  const params = new URLSearchParams();
  const search = $('sessionsearch')?.value.trim();
  if (search) params.set('search', search);
  if ($('showarchived')?.checked) params.set('include_archived', 'true');
  if (older && sessionsCursor) params.set('before_seq', sessionsCursor);
  const data = await api(`/sessions${params.toString() ? `?${params}` : ''}`);
  if (!token || myEpoch !== epoch) return;
  if (!older) $('sessionlist').replaceChildren();
  for (const item of data.sessions) {
    if ($('sessionlist').childNodes.length >= MAX_RENDERED_SESSIONS) break;
    const entry = node('div', undefined, 'session-entry');
    const button = node('button', undefined, 'secondary session-open'); button.type = 'button';
    button.append(node('strong', item.title || item.preview || 'Conversation'), node('span', `${item.scope} · ${item.message_count} messages${item.archived_at ? ' · archived' : ''}`, 'muted'));
    button.addEventListener('click', async () => {
      if (busy || pending) return notice('Let the current message finish before switching conversations.', true);
      session = item.id; scope = item.scope; epoch++; $('scope').value = scope; persistSession();
      // P2-T02: in the wide three-pane layout the sidebar stays put; only the mobile drawer closes.
      if (window.matchMedia('(max-width: 920px)').matches) { $('sessionhistory').open = false; $('sidebar').classList.remove('open'); $('drawerbg').classList.remove('show'); }
      await loadHistory().catch(e => notice(e.message, true)); refreshScopeSetup().catch(() => {}); if (pending) await resumeRecording();
    });
    const actions = node('div', undefined, 'session-actions');
    const rename = node('button', 'Rename', 'secondary'); rename.type = 'button';
    rename.addEventListener('click', async () => {
      const next = window.prompt('Conversation name', item.title || 'Conversation');
      if (next === null) return;
      try { await apiPatch(`/sessions/${encodeURIComponent(item.id)}`, { title: next }); notice('Conversation renamed.'); await loadSessions(); }
      catch (error) { notice(error.message, true); }
    });
    const archive = node('button', item.archived_at ? 'Restore' : 'Archive', 'secondary'); archive.type = 'button';
    archive.addEventListener('click', async () => {
      try { await apiPatch(`/sessions/${encodeURIComponent(item.id)}`, { archived: !item.archived_at }); notice(item.archived_at ? 'Conversation restored.' : 'Conversation archived.'); await loadSessions(); }
      catch (error) { notice(error.message, true); }
    });
    const fork = node('button', 'Fork', 'secondary'); fork.type = 'button';
    fork.addEventListener('click', async () => {
      if (busy || pending) return notice('Let the current message finish before forking.', true);
      try {
        const copy = await api(`/sessions/${encodeURIComponent(item.id)}/fork`, {});
        session = copy.id; scope = copy.scope; epoch++; $('scope').value = scope; persistSession(); historyCursor = null;
        await loadHistory(); await loadSessions(); notice('Fork created. The original conversation was not changed.');
      } catch (error) { notice(error.message, true); }
    });
    actions.append(rename, archive, fork); entry.append(button, actions); $('sessionlist').append(entry);
  }
  if (!$('sessionlist').childNodes.length) $('sessionlist').append(node('p', 'Your saved conversations will appear here.', 'muted'));
  sessionsCursor = data.next_before_seq; $('oldersessions').hidden = !data.has_more;
}
function setBusy(value) { busy = value; $('send').disabled = value; $('newchat').disabled = value; $('checkrecording').disabled = value; }
function accepted(receipt) {
  captureLabel(stateLabels[receipt.state] || 'Sent');
  // Clear only the submitted draft, never text typed after it.
  if (pendingPrompt !== null && $('prompt').value === pendingPrompt) $('prompt').value = '';
}

// P7-T03: answers are rendered only from persisted generation rows. The receipt poll still
// controls admission and terminal cleanup, but its `response` field is never used to paint the
// live answer. The cursor is kept with the pending request identity so a reload resumes after
// the last row this tab handled instead of replaying it into the UI.
const generationStream = { controller: null, retry: null, sessionId: null, requestId: null, cursor: 0, failures: 0, view: null };
function saveGenerationCursor() {
  if (!pending || pending.request_id !== generationStream.requestId) return;
  pending.generation_cursor = generationStream.cursor;
  sessionStorage.setItem('harness_pending', JSON.stringify(pending));
}
function closeGenerationStream(reset = true) {
  if (generationStream.retry) clearTimeout(generationStream.retry);
  if (generationStream.controller) generationStream.controller.abort();
  generationStream.retry = null; generationStream.controller = null;
  if (reset) Object.assign(generationStream, { sessionId: null, requestId: null, cursor: 0, failures: 0, view: null });
}
function generationView(requestId) {
  if (generationStream.view?.isConnected && generationStream.view.dataset.requestId === requestId) return generationStream.view;
  const box = node('article', undefined, 'message assistant generation-message generating');
  box.dataset.requestId = requestId; box.dataset.state = 'generating'; box.setAttribute('aria-live', 'polite');
  box.append(node('strong', 'Harness'), node('span', 'Waiting for the recorded answer…', 'generation-content'), node('span', 'Thinking…', 'muted generation-state'));
  const empty = $('log').querySelector(':scope > .empty'); if (empty) empty.remove();
  $('log').append(box); generationStream.view = box;
  $('chatscroll').scrollTop = $('chatscroll').scrollHeight;
  return box;
}
function renderGenerationEvent(event) {
  if (!event || event.request_id !== generationStream.requestId) return;
  const state = event.state;
  if (!['generating','complete','failed','interrupted'].includes(state)) return;
  const box = generationView(event.request_id);
  box.classList.remove('generating','complete','failed','interrupted'); box.classList.add(state); box.dataset.state = state;
  const content = box.querySelector('.generation-content'); const label = box.querySelector('.generation-state');
  if (state === 'chunk') { if (box.dataset.chunked !== 'true') { content.textContent = ''; box.dataset.chunked = 'true'; } content.textContent += typeof event.content === 'string' ? event.content : ''; label.textContent = 'Thinking\u2026'; }
  if (state === 'complete') { content.textContent = typeof event.content === 'string' ? event.content : ''; label.textContent = 'Done · saved response'; }
  else if (state === 'failed') { content.textContent = 'The provider failed before an answer was saved.'; label.textContent = `Failed${event.error_code ? ` · ${event.error_code}` : ''}`; }
  else if (state === 'interrupted') { content.textContent = 'The server restarted before an answer was saved. Nothing was resent.'; label.textContent = 'Interrupted'; }
  else { content.textContent = 'Waiting for the recorded answer…'; label.textContent = 'Thinking…'; }
  $('chatscroll').scrollTop = $('chatscroll').scrollHeight;
}
function applyGenerationEvent(event) {
  const seq = Number(event?.seq);
  if (!Number.isFinite(seq) || seq <= generationStream.cursor) return;
  generationStream.cursor = seq; saveGenerationCursor(); renderGenerationEvent(event);
}
function consumeGenerationFrame(frame) {
  if (!frame.trim() || frame.startsWith(':')) return;
  let seq = null; const data = [];
  for (const line of frame.split('\n')) {
    if (line.startsWith('id:')) seq = Number(line.slice(3).trim());
    else if (line.startsWith('data:')) data.push(line.slice(5).trimStart());
  }
  if (!data.length) return;
  try { const event = JSON.parse(data.join('\n')); if (seq !== null && event.seq === undefined) event.seq = seq; applyGenerationEvent(event); } catch {}
}
async function pollGeneration(sessionId) {
  if (!token || !sessionId || generationStream.sessionId !== sessionId) return;
  const data = await api(`/generation?session_id=${encodeURIComponent(sessionId)}&after_seq=${generationStream.cursor}`);
  for (const event of data.events || []) applyGenerationEvent(event);
}
async function followGenerationStream(sessionId, requestId) {
  if (!token || !sessionId || !requestId || generationStream.controller) return;
  if (generationStream.sessionId !== sessionId || generationStream.requestId !== requestId) {
    closeGenerationStream();
    generationStream.sessionId = sessionId; generationStream.requestId = requestId;
    generationStream.cursor = Number(pending?.request_id === requestId ? pending.generation_cursor : 0) || 0;
  }
  const controller = new AbortController(); const myEpoch = epoch;
  generationStream.controller = controller;
  try {
    const response = await fetch(`/generation/stream?session_id=${encodeURIComponent(sessionId)}&after_seq=${generationStream.cursor}`,
      { headers: { 'Authorization': `Bearer ${token}` }, signal: controller.signal });
    if (!response.ok || !response.body) throw new Error(`Generation stream unavailable (${response.status})`);
    generationStream.failures = 0;
    const reader = response.body.getReader(); const decoder = new TextDecoder(); let buffer = '';
    for (;;) {
      const chunk = await reader.read(); if (chunk.done) break;
      if (!token || myEpoch !== epoch || pending?.request_id !== requestId) return;
      buffer += decoder.decode(chunk.value, { stream: true });
      const frames = buffer.split('\n\n'); buffer = frames.pop();
      for (const frame of frames) consumeGenerationFrame(frame);
    }
    buffer += decoder.decode();
    for (const frame of buffer.split('\n\n')) consumeGenerationFrame(frame);
  } catch (error) { if (error?.name !== 'AbortError') generationStream.failures++; }
  finally { if (generationStream.controller === controller) generationStream.controller = null; controller.abort(); }
  if (token && myEpoch === epoch && pending?.request_id === requestId && generationStream.failures < 3 && !generationStream.controller) {
    generationStream.retry = setTimeout(() => { generationStream.retry = null; followGenerationStream(sessionId, requestId); }, Math.min(1000 * 2 ** generationStream.failures, 5000));
  }
}
async function followReceipt(first, myEpoch) {
  let data = first;
  for (let i=0; i<120; i++) {
    if (!token || myEpoch !== epoch) return;
    accepted(data);
    const terminal = ['complete','failed','interrupted'].includes(data.state);
    if (i === 0 && !terminal) { await loadHistory(); notice('Your message is saved. You can reconnect later to check the answer.'); }
    // The activity rail and answer use independent durable cursors.
    if (data.session_id) {
      followActivityStream(data.session_id).catch(() => {});
      followGenerationStream(data.session_id, data.request_id).catch(() => {});
      await pollGeneration(data.session_id).catch(() => {});
    }
    await refreshAgentTurn(data).catch(() => {});
    if (terminal) {
      closeGenerationStream(false); rememberPending(null); closeActivityStream(); $('retryrequest').hidden = true;
      await loadHistory(); await loadSessions(); closeGenerationStream();
      if (data.state === 'complete') notice(data.redacted ? 'Answer saved. Sensitive-looking input was filtered before saving.' : 'Answer saved on this device.');
      else notice(data.state === 'interrupted' ? 'Your message is saved. The server restarted before the answer completed; nothing was resent.' : 'Your message is saved, but the answer did not complete.', true);
      return;
    }
    await new Promise(resolve => setTimeout(resolve, 1000));
    if (!token || myEpoch !== epoch) return;
    data = await api(`/chat/requests/${encodeURIComponent(data.request_id)}`);
  }
  notice('Your message is saved. The answer is still pending; use Check status to refresh.');
}
async function resumeRecording() {
  if (!pending || busy || !token) return;
  const myEpoch = epoch; setBusy(true);
  try { const data = await api(`/chat/requests/${encodeURIComponent(pending.request_id)}`); if (myEpoch === epoch && token) await followReceipt(data, myEpoch); }
  catch (error) {
    if (myEpoch === epoch && token) {
      captureLabel('Needs checking');
      notice(error instanceof ApiError && error.is('not_found') ? 'No receipt found yet. Nothing was resent. If your draft is still in this tab, Retry same message safely reuses its original request.' : 'Connection lost. The latest status is unknown—not necessarily lost. Check again; nothing will be resent automatically.', true);
      $('retryrequest').hidden = pendingPrompt === null;
    }
  } finally { if (myEpoch === epoch) setBusy(false); }
}
// P14-T01: an explicit Stop records durable cancellation intent; the server also terminates any
// process group the turn is still blocked on. The turn then lands on `interrupted`, a recorded
// terminal state — it is never silently treated as a completed answer.
async function cancelPending() {
  if (!pending) return;
  const requestId = pending.request_id, myEpoch = epoch;
  $('cancelrequest').disabled = true;
  try {
    const receipt = await api(`/chat/requests/${encodeURIComponent(requestId)}/cancel`, {});
    if (!token || myEpoch !== epoch) return;
    notice(receipt.state === 'interrupted'
      ? 'Cancelled. The turn stopped at its last recorded boundary; no side effect was replayed.'
      : 'Cancellation requested. The turn will stop at the next recorded boundary.');
  } catch (error) {
    notice(`Could not cancel: ${error.message}`, true);
  } finally { $('cancelrequest').disabled = false; }
}
// P14-T01: the server admits a retry only from a recorded non-mutating boundary. It returns the
// new receipt; this tab adopts it as its active turn and follows it like any other.
async function retryFromBoundary(requestId, button) {
  if (busy) return notice('Let the current message finish before retrying.', true);
  const myEpoch = epoch;
  if (button) button.disabled = true;
  try {
    const receipt = await api(`/chat/requests/${encodeURIComponent(requestId)}/retry`, {});
    if (!token || myEpoch !== epoch) return;
    rememberPending({ request_id: receipt.request_id, session_id: receipt.session_id, scope: receipt.scope, generation_cursor: 0 });
    pendingPrompt = null;
    notice('Retrying from the last recorded non-mutating boundary. Nothing was replayed.');
    await loadHistory();
    setBusy(true);
    try { await followReceipt(receipt, myEpoch); } finally { if (myEpoch === epoch) setBusy(false); }
  } catch (error) {
    notice((error instanceof ApiError && error.is('conflict') ? 'Retry refused: ' : 'Retry failed: ') + error.message, true);
  } finally { if (button) button.disabled = false; }
}
async function sendAttempt(retry = false) {
  if (busy) return;
  if (pending && !retry) return resumeRecording();
  const prompt = retry ? pendingPrompt : $('prompt').value;
  if (typeof prompt !== 'string' || !prompt.trim() || new TextEncoder().encode(prompt).length > 16000) return notice('Message must contain 1–16000 UTF-8 bytes.', true);
  if (!retry) { pendingPrompt = prompt; rememberPending({request_id:crypto.randomUUID(),session_id:session,scope,generation_cursor:0}); }
  let admitted = false;
  const myEpoch = epoch; setBusy(true); captureLabel('Sending…');
  try {
    const body = {prompt};
    if (pending) for (const key of PENDING_FIELDS) { if (typeof pending[key] === 'string') body[key] = pending[key]; }
    const result = await api('/chat/submit', body); admitted = true;
    if (token && myEpoch === epoch) await followReceipt(result, myEpoch);
  } catch (error) {
    if (token && myEpoch === epoch) {
      // Only definite validation/admission rejections clear the pending identity.
      // 5xx/network/parse failures may occur AFTER a commit: keep the receipt ID.
      // `notAdmitted` is that same set expressed as codes; see NOT_ADMITTED_CODES in api.js.
      if (!admitted && !retry && error instanceof ApiError && error.notAdmitted) {
        rememberPending(null); captureLabel('Message not accepted'); notice(error.message + ' Your draft remains in this tab.', true);
      } else {
        captureLabel('Needs checking'); notice('Could not confirm the result. Use Check status before sending again. Your draft stays in this tab.', true);
        $('retryrequest').hidden = pendingPrompt === null;
      }
    }
  } finally { if (myEpoch === epoch) setBusy(false); }
}
$('authform').addEventListener('submit', async event => {
  event.preventDefault(); let masterToken = $('token').value.trim(); $('token').value = ''; const myEpoch = epoch;
  try {
    token = await exchangeBrowserSession(masterToken); masterToken = '';
    const health = await api('/health'); if (myEpoch !== epoch) return;
    if (!health.ready) throw new Error('Harness is not ready');
    if (health.commit !== EXPECTED_SERVER_COMMIT) throw new Error('This page is stale; reload before connecting');
    $('auth').hidden = true; $('workspace').hidden = false; setConnectionState('connected', 'Connected');
    notice('Connected — pick up where you left off.'); persistSession(); await loadHistory(); await loadSessions(); await refreshStatus(); await refreshScopeSetup();
    rememberPending(pending); if (pending) await resumeRecording();
  } catch (error) { token = ''; $('workspace').hidden = true; $('auth').hidden = false; notice(error.message, true); }
  finally { masterToken = ''; }
});
$('lock').addEventListener('click', () => {
  token = ''; epoch++; setBusy(false); pendingPrompt = null; closeGenerationStream(); closeActivityStream();
  $('workspace').hidden = true; $('auth').hidden = false; setConnectionState('locked', 'Locked');
  for (const id of ['log','candidates','jobs','recalled','sessionlist','retrieval-list']) $(id).replaceChildren();
  $('modelnames').textContent = ''; $('stats').textContent = ''; $('mainmodel').value = ''; $('extractmodel').value = ''; $('verificationmodel').value = ''; $('prompt').value = ''; $('file').value = ''; $('consent').checked = false;
  clearProviderEditor();
  $('provider-list').replaceChildren();
  $('provider-status').textContent = '';
  $('setup-banner').hidden = true; $('scopelist').replaceChildren();
  notice('Locked. Unsaved draft text was cleared; recorded work stays on the server.');
});
$('newchat').addEventListener('click', () => {
  if (busy || pending) return notice('Let the current message finish before starting another conversation.', true);
  const next = $('scope').value.trim();
  if (!/^[A-Za-z0-9_.:-]{1,80}$/.test(next)) return notice('Use a scope of 1–80 letters, digits, _, -, . or :.', true);
  scope = next; session = crypto.randomUUID(); epoch++; persistSession(); historyCursor = null; $('oldermessages').hidden = true;
  $('log').replaceChildren(node('p', 'New conversation. Earlier work is available in History.', 'empty')); $('recalltrace').hidden = true; captureLabel(''); notice(`New conversation in ${scope}.`); refreshScopeSetup().catch(() => {});
});
$('chatform').addEventListener('submit', event => { event.preventDefault(); sendAttempt(); });
$('checkrecording').addEventListener('click', resumeRecording);
$('retryrequest').addEventListener('click', () => sendAttempt(true));
$('cancelrequest').addEventListener('click', () => cancelPending());
$('leavepending').addEventListener('click', () => {
  if (!pending || !window.confirm('Start a new conversation without resending or cancelling this request? Any saved work remains in History. Unsaved draft text will be cleared.')) return;
  epoch++; setBusy(false); closeGenerationStream(); closeActivityStream(); rememberPending(null); $('retryrequest').hidden = true; $('prompt').value = '';
  session = crypto.randomUUID(); persistSession(); historyCursor = null; $('oldermessages').hidden = true;
  $('log').replaceChildren(node('p', 'New conversation. Check History later for the previous answer.', 'empty')); captureLabel(''); notice('Nothing was resent. Any saved work remains in History.');
});
$('oldermessages').addEventListener('click', async () => { $('oldermessages').disabled = true; try { await loadHistory(true); } catch(e) { notice(e.message,true); } finally { $('oldermessages').disabled = false; } });
$('oldersessions').addEventListener('click', () => loadSessions(true).catch(e=>notice(e.message,true)));
$('sessionhistory').addEventListener('toggle', () => { if ($('sessionhistory').open) loadSessions().catch(e=>notice(e.message,true)); });
$('sessionsearch').addEventListener('input', () => { sessionsCursor = null; loadSessions().catch(e=>notice(e.message,true)); });
$('showarchived').addEventListener('change', () => { sessionsCursor = null; loadSessions().catch(e=>notice(e.message,true)); });
async function refreshCandidateViews() {
  await Promise.all([loadCandidates(), refreshInlineSuggestions(), refreshStatus()]);
}
function candidateCard(c) {
  const card = node('article', undefined, `candidate${c.priority === 'high' ? ' high-priority' : ''}`);
  const title = node('h3'); title.append(document.createTextNode(c.key));
  if (c.priority === 'high') title.append(node('span', 'Correction', 'priority-badge'));
  card.append(title, node('p', `${c.category} · ${c.scope} · based on revision ${c.expected_revision}`, 'muted'), node('p', `Previously: ${c.old_value ?? 'Not remembered'}`), node('p', `Proposed: ${c.value}`), node('blockquote', c.evidence?.quote || 'Legacy import: original evidence is unavailable.'));
  const controls = node('div', undefined, 'row');
  const setDisabled = value => { for (const button of controls.querySelectorAll('button')) button.disabled = value; };
  const resolve = async confirm => {
    setDisabled(true);
    try { const out = await api('/memory/confirm', { confirmation_id: c.id, confirm, scope: c.scope }); notice(confirm ? `Memory ${out.status}.` : 'Suggestion dismissed.'); await refreshCandidateViews(); }
    catch (error) { notice(error.message, true); setDisabled(false); }
  };
  const save = node('button', 'Save'); save.type = 'button'; save.addEventListener('click', () => resolve(true));
  const edit = node('button', 'Edit', 'secondary'); edit.type = 'button'; edit.addEventListener('click', () => {
    if (card.querySelector('.candidate-editor')) return;
    const form = node('form', undefined, 'candidate-editor'); const label = node('label', 'Edit proposed memory'); const textarea = node('textarea'); textarea.value = c.value; textarea.maxLength = 1000; textarea.required = true;
    const actions = node('div', undefined, 'row'); const apply = node('button', 'Apply edit'); apply.type = 'submit'; const cancel = node('button', 'Cancel', 'secondary'); cancel.type = 'button'; cancel.addEventListener('click', () => form.remove()); actions.append(apply, cancel); form.append(label, textarea, actions);
    form.addEventListener('submit', async event => { event.preventDefault(); apply.disabled = true; cancel.disabled = true; try { await api(`/memory/candidates/${encodeURIComponent(c.id)}/edit`, { value: textarea.value, scope: c.scope }); notice('Suggestion edited. Review it before saving.'); await refreshCandidateViews(); } catch (error) { notice(error.message, true); apply.disabled = false; cancel.disabled = false; } });
    card.append(form); textarea.focus();
  });
  const dismiss = node('button', 'Dismiss', 'secondary'); dismiss.type = 'button'; dismiss.addEventListener('click', () => resolve(false));
  // P15-T02: rehearse retrieval before approving. The server rolls the trial approval back, so
  // this button changes nothing; it reports which memories retrieval would send, not what the
  // model would answer.
  const preview = node('button', 'Preview retrieval', 'secondary'); preview.type = 'button';
  preview.addEventListener('click', async () => {
    card.querySelector('.retrieval-preview')?.remove();
    preview.disabled = true;
    const box = node('div', undefined, 'retrieval-preview');
    try {
      const out = await api('/memory/retrieval/preview', { prompt: c.evidence?.quote || c.value, scope: c.scope, candidate_id: c.id });
      const added = Array.isArray(out.added) ? out.added : [];
      const removed = Array.isArray(out.removed) ? out.removed : [];
      box.append(node('strong', added.length || removed.length ? `Retrieval would change: +${added.length} / -${removed.length}` : 'Retrieval would not change'));
      for (const row of added) box.append(node('span', `+ ${String(row.key ?? 'memory')} · score ${Number(row.total_score || 0).toFixed(3)}`, 'muted small'));
      for (const row of removed) box.append(node('span', `- ${String(row.key ?? 'memory')} · score ${Number(row.total_score || 0).toFixed(3)}`, 'muted small'));
      box.append(node('span', String(out.note || ''), 'muted small'));
      card.append(box);
    } catch (error) { notice(error.message, true); }
    finally { preview.disabled = false; }
  });
  controls.append(save, edit, preview, dismiss); card.append(controls); return card;
}
async function loadCandidates() {
  const myEpoch = epoch; const data = await api(`/memory/candidates?scope=${encodeURIComponent(scope)}&imports_only=true`);
  if (!token || myEpoch !== epoch) return;
  $('candidates').replaceChildren();
  if (!data.candidates.length) $('candidates').append(node('p', 'No pending suggestions from transcript imports in this scope.', 'empty'));
  for (const candidate of data.candidates) $('candidates').append(candidateCard(candidate));
}
async function refreshInlineSuggestions() {
  const trays = [...document.querySelectorAll('.suggestion-tray[data-request-id]')]; if (!trays.length || !token) return;
  const myEpoch = epoch; const data = await api(`/memory/candidates?scope=${encodeURIComponent(scope)}&chat_only=true`); if (!token || myEpoch !== epoch) return;
  const grouped = new Map();
  for (const candidate of data.candidates) { if (!candidate.request_id) continue; const list = grouped.get(candidate.request_id) || []; list.push(candidate); grouped.set(candidate.request_id, list); }
  for (const tray of trays) {
    const candidates = grouped.get(tray.dataset.requestId) || []; tray.replaceChildren(); tray.hidden = !candidates.length;
    if (candidates.length) { tray.append(node('strong', 'Suggested memories from this turn')); for (const candidate of candidates) tray.append(candidateCard(candidate)); }
  }
}
async function loadProcesses() {
  const myEpoch = epoch; const data = await api('/processes'); if (!token || myEpoch !== epoch) return;
  const list = $('processes'); list.replaceChildren();
  if (!data.processes?.length) { list.append(node('p', 'No registered processes.', 'empty')); return; }
  for (const process of data.processes) {
    const card = node('div', undefined, 'job');
    card.append(node('strong', `${process.summary || 'Detached process'} · PID ${process.pid}`));
    card.append(node('p', `${process.scope} · request ${process.request_id}`, 'muted'));
    if (process.log) card.append(node('p', `Log: ${process.log}`, 'mono small'));
    const stop = node('button', 'Stop registered process', 'secondary'); stop.type = 'button';
    stop.addEventListener('click', async () => {
      if (!window.confirm(`Stop PID ${process.pid}? Only this registered process will be targeted.`)) return;
      stop.disabled = true;
      try { await api(`/processes/${encodeURIComponent(process.pid)}/stop`, {}); notice(`Stopped registered process ${process.pid}.`); await loadProcesses(); }
      catch (error) { notice(error.message, true); stop.disabled = false; }
    });
    card.append(stop); list.append(card);
  }
}
async function loadGitState() {
  const myEpoch = epoch; const data = await api(`/git/state?scope=${encodeURIComponent(scope)}`); if (!token || myEpoch !== epoch) return;
  const status = data.status || {}; const diff = data.diff_stat || {};
  $('git-trust').textContent = data.trusted ? `Read-only Git evidence for ${data.scope}; no commit, restore, or push was performed.` : 'Git state could not be trusted.';
  $('git-status').textContent = `STATUS\n${status.output || '(no status output)'}\n\nDIFF STAT\n${diff.output || '(no diff stat)'}`;
}
async function loadJobs() {
  const myEpoch = epoch; const data = await api('/jobs'); if (!token || myEpoch !== epoch) return;
  $('jobs').replaceChildren(); if (!data.jobs.length) $('jobs').append(node('p', 'No processing jobs yet.', 'empty'));
  for (const j of data.jobs) {
    const card = node('div', undefined, 'job'); card.append(node('strong', `${j.status} · ${j.scope}`), node('p', `Attempts: ${j.attempts}`, 'muted'));
    if (j.error) card.append(node('p', j.error));
    if (j.status === 'failed') { const button = node('button', 'Retry extraction', 'secondary'); button.type = 'button'; button.addEventListener('click', async () => { button.disabled = true; try { await api(`/jobs/${encodeURIComponent(j.id)}/retry`, {}); await loadJobs(); } catch (error) { notice(error.message, true); button.disabled = false; } }); card.append(button); }
    $('jobs').append(card);
  }
}
$('importform').addEventListener('submit', async event => {
  event.preventDefault(); const file = $('file').files[0]; if (!file || file.size > 1048576) return notice('Select a transcript of no more than 1 MiB.', true);
  $('importbutton').disabled = true;
  try { const request = { name: file.name, content: await file.text(), scope }; if ($('format').value) request.format = $('format').value; const result = await api('/memory/ingest', request); notice(result.duplicate ? 'This source was already imported. No duplicate jobs were created.' : `Queued ${result.chunks_queued} chunks. ${result.warnings.join(' ')}`); await loadJobs(); await refreshStatus(); }
  catch (error) { notice(error.message, true); } finally { $('importbutton').disabled = false; }
});
let providerState = { selected: null, providers: [] };
const PROVIDER_ID_RE = /^[a-z0-9][a-z0-9_-]{0,63}$/;
function providerStatus(text, error = false) {
  $('provider-status').textContent = text;
  $('provider-status').classList.toggle('error', error);
}
function clearProviderEditor() {
  const form = $('providerform');
  if (!form) return;
  form.reset();
  $('provider-editing-original').value = '';
  $('providerid').disabled = false;
  $('providerkey').value = '';
  $('providerpaste').value = '';
  $('provider-form-title').textContent = 'Add provider';
  $('provider-key-hint').textContent = 'Required for a new provider. When editing, leave blank to keep the stored key.';
  form.hidden = true;
}
function openProviderEditor(provider = null) {
  clearProviderEditor();
  $('providerform').hidden = false;
  if (provider) {
    $('provider-form-title').textContent = `Edit ${String(provider.id)}`;
    $('provider-editing-original').value = String(provider.id);
    $('providerid').value = String(provider.id);
    $('providerid').disabled = true;
    $('providerbaseurl').value = String(provider.baseUrl || '');
    $('providerapi').value = String(provider.api || 'openai-completions');
    $('providerdiscovery').value = String(provider.discovery?.type || 'proxy');
    $('provider-key-hint').textContent = provider.keyPresent
      ? 'A key is stored. Leave this blank to keep it, or enter a replacement.'
      : 'No stored key is present. Enter a key before saving.';
  }
  $('providerid').focus();
}
function renderProviders(data) {
  providerState = {
    selected: typeof data?.selected === 'string' ? data.selected : null,
    providers: Array.isArray(data?.providers) ? data.providers : [],
  };
  const list = $('provider-list');
  list.replaceChildren();
  if (!providerState.providers.length) {
    list.append(node('p', 'No provider profiles are available.', 'empty'));
    return;
  }
  for (const provider of providerState.providers) {
    const card = node('article', undefined, 'provider-card');
    const heading = node('div', undefined, 'provider-card-head');
    const title = node('strong', String(provider.id || 'unnamed provider'));
    heading.append(title);
    if (provider.selected) heading.append(node('span', 'Selected', 'badge'));
    else heading.append(node('span', 'Available', 'badge subtle'));
    card.append(heading);
    card.append(
      node('p', String(provider.baseUrl || ''), 'mono small'),
      node('p', `${String(provider.api || 'unknown adapter')} · discovery ${String(provider.discovery?.type || 'unknown')} · version ${String(provider.version ?? '?')}`, 'muted small'),
      node('p', provider.keyPresent ? 'API key: stored (hidden)' : 'API key: missing', 'muted small'),
    );
    const actions = node('div', undefined, 'row actions');
    const test = node('button', 'Test discovery', 'secondary'); test.type = 'button';
    test.addEventListener('click', async () => {
      test.disabled = true; providerStatus(`Testing ${String(provider.id)}…`);
      try {
        const result = await api(`/providers/${encodeURIComponent(provider.id)}/test`, {});
        const state = String(result.discovery?.status || 'unknown');
        const count = Number(result.discovery?.modelCount || 0);
        providerStatus(`Tested ${String(provider.id)}: discovery ${state}${state === 'available' ? ` · ${count} model ID${count === 1 ? '' : 's'}` : ''}. Generation/tools/streaming/usage remain untested.`);
      } catch (error) { providerStatus(`Provider test failed: ${error.message}`, true); }
      finally { test.disabled = false; }
    });
    actions.append(test);
    if (!provider.selected) {
      const select = node('button', 'Select', 'secondary'); select.type = 'button';
      select.addEventListener('click', async () => {
        select.disabled = true;
        try {
          await api(`/providers/${encodeURIComponent(provider.id)}/select`, {});
          providerStatus(`Selected ${String(provider.id)} for newly admitted work.`);
          await loadProviders();
        } catch (error) { providerStatus(error.message, true); select.disabled = false; }
      });
      actions.append(select);
    }
    if (provider.source === 'saved') {
      const edit = node('button', 'Edit', 'secondary'); edit.type = 'button'; edit.addEventListener('click', () => openProviderEditor(provider)); actions.append(edit);
      const remove = node('button', 'Delete', 'secondary danger'); remove.type = 'button';
      if (provider.selected) {
        remove.disabled = true;
        remove.title = 'Select another provider before deleting this one.';
      } else remove.addEventListener('click', async () => {
        if (!window.confirm(`Delete provider “${String(provider.id)}” from future selection? Historical admitted work keeps its pinned provider version.`)) return;
        remove.disabled = true;
        try { await apiDelete(`/providers/${encodeURIComponent(provider.id)}`); providerStatus(`Deleted ${String(provider.id)} from future selection.`); await loadProviders(); }
        catch (error) { providerStatus(error.message, true); remove.disabled = false; }
      });
      actions.append(remove);
    } else {
      const environment = node('span', 'Startup environment provider · edit in server environment', 'muted small');
      actions.append(environment);
    }
    card.append(actions);
    list.append(card);
  }
}
async function loadProviders() {
  providerStatus('Loading providers…');
  try {
    const data = await api('/providers');
    renderProviders(data);
    providerStatus(providerState.selected ? `Selected provider: ${providerState.selected}` : 'No selected provider.');
  } catch (error) {
    providerStatus(`Could not load providers. ${error.message}`, true);
    throw error;
  }
}
function providerScalar(value) {
  const v = value.trim();
  if ((v.startsWith('"') && v.endsWith('"')) || (v.startsWith("'") && v.endsWith("'"))) return v.slice(1, -1);
  return v;
}
function validateProviderPasteObject(raw) {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) throw new Error('Paste must contain one provider object.');
  let id, config;
  if (typeof raw.id === 'string') {
    const allowed = new Set(['id','baseUrl','apiKey','api','discovery']);
    for (const key of Object.keys(raw)) if (!allowed.has(key)) throw new Error('Paste contains an unsupported field.');
    id = raw.id; config = raw;
  } else {
    const keys = Object.keys(raw);
    if (keys.length !== 1) throw new Error('Paste must contain exactly one provider ID.');
    id = keys[0]; config = raw[id];
  }
  if (!PROVIDER_ID_RE.test(id)) throw new Error('Provider ID must use lowercase letters, digits, _ or - and start with a letter or digit.');
  if (!config || typeof config !== 'object' || Array.isArray(config)) throw new Error('Provider configuration must be an object.');
  const allowed = new Set(['baseUrl','apiKey','api','discovery', ...(config === raw ? ['id'] : [])]);
  for (const key of Object.keys(config)) if (!allowed.has(key)) throw new Error('Paste contains an unsupported field.');
  if (typeof config.baseUrl !== 'string' || !config.baseUrl || config.baseUrl.length > 2048) throw new Error('baseUrl is required and must be bounded.');
  if (config.api !== 'openai-completions') throw new Error('Only api: openai-completions is supported.');
  if (!config.discovery || typeof config.discovery !== 'object' || Array.isArray(config.discovery) || Object.keys(config.discovery).some(key => key !== 'type') || config.discovery.type !== 'proxy') throw new Error('Only discovery.type: proxy is supported.');
  if (config.apiKey !== undefined && (typeof config.apiKey !== 'string' || config.apiKey.length > 4096)) throw new Error('apiKey must be a bounded string.');
  return { id, baseUrl: config.baseUrl, apiKey: config.apiKey || '', api: config.api, discovery: { type: config.discovery.type } };
}
function parseProviderPaste(text) {
  if (typeof text !== 'string' || !text.trim() || new TextEncoder().encode(text).length > 8192) throw new Error('Paste must contain 1–8192 UTF-8 bytes.');
  const trimmed = text.trim();
  if (trimmed.startsWith('{')) return validateProviderPasteObject(JSON.parse(trimmed));
  if (text.includes('\t')) throw new Error('YAML tabs are not supported.');
  const lines = text.split(/\r?\n/).filter(line => line.trim());
  if (lines.length < 5 || lines.length > 8) throw new Error('YAML must use the bounded provider template.');
  const root = lines.shift().match(/^([a-z0-9][a-z0-9_-]{0,63}):\s*$/);
  if (!root) throw new Error('YAML must start with one provider ID.');
  const config = {};
  let inDiscovery = false;
  for (const line of lines) {
    const discoveryHead = line.match(/^  discovery:\s*$/);
    if (discoveryHead) {
      if (inDiscovery || config.discovery) throw new Error('Duplicate discovery block.');
      config.discovery = {}; inDiscovery = true; continue;
    }
    if (inDiscovery) {
      const type = line.match(/^    type:\s*(.+?)\s*$/);
      if (!type || config.discovery.type !== undefined) throw new Error('Only discovery.type is allowed.');
      config.discovery.type = providerScalar(type[1]); inDiscovery = false; continue;
    }
    const field = line.match(/^  (baseUrl|apiKey|api):\s*(.*?)\s*$/);
    if (!field || config[field[1]] !== undefined) throw new Error('YAML contains an unsupported or duplicate field.');
    config[field[1]] = providerScalar(field[2]);
  }
  return validateProviderPasteObject({ [root[1]]: config });
}
$('provider-add').addEventListener('click', () => openProviderEditor());
$('provider-cancel').addEventListener('click', () => { clearProviderEditor(); providerStatus('Provider edit cancelled.'); });
$('provider-parse').addEventListener('click', () => {
  try {
    const parsed = parseProviderPaste($('providerpaste').value);
    $('providerid').disabled = false;
    $('provider-editing-original').value = '';
    $('providerid').value = parsed.id;
    $('providerbaseurl').value = parsed.baseUrl;
    $('providerapi').value = parsed.api;
    $('providerdiscovery').value = parsed.discovery.type;
    $('providerkey').value = parsed.apiKey;
    $('providerpaste').value = '';
    $('provider-form-title').textContent = 'Review parsed provider';
    providerStatus('Parsed into the form. Review it, then save explicitly.');
  } catch (error) { providerStatus(`Could not parse provider configuration. ${error.message}`, true); }
});
$('providerform').addEventListener('submit', async event => {
  event.preventDefault();
  const id = $('providerid').value.trim();
  const editing = $('provider-editing-original').value;
  const key = $('providerkey').value;
  if (!PROVIDER_ID_RE.test(id)) return providerStatus('Provider ID must use lowercase letters, digits, _ or - and start with a letter or digit.', true);
  if (editing && editing !== id) return providerStatus('Provider ID cannot be renamed while editing. Add a new provider instead.', true);
  if (!editing && !key) return providerStatus('A new provider requires an API key.', true);
  const payload = {
    id,
    baseUrl: $('providerbaseurl').value.trim(),
    api: $('providerapi').value,
    discovery: { type: $('providerdiscovery').value },
  };
  if (key) payload.apiKey = key;
  $('provider-save').disabled = true;
  try {
    await api('/providers', payload);
    const message = editing ? `Saved a new version of ${id}. The stored key was ${key ? 'replaced' : 'kept'}.` : `Saved provider ${id}. Select it explicitly when ready.`;
    clearProviderEditor();
    providerStatus(message);
    await loadProviders();
  } catch (error) { providerStatus(`Provider was not saved. ${error.message}`, true); }
  finally { $('provider-save').disabled = false; $('providerkey').value = ''; }
});
$('settingsform').addEventListener('submit', async event => { event.preventDefault(); try { await api('/config', { main: $('mainmodel').value.trim(), extraction: $('extractmodel').value.trim(), verification: $('verificationmodel').value.trim() }); notice('Model settings saved.'); } catch (error) { notice(error.message, true); } });
$('loadmodels').addEventListener('click', async () => { try { const data = await api('/models'); $('modelnames').textContent = data.data.map(m => String(m.id)).join('\n'); } catch (error) { notice(error.message, true); } });
$('refreshmemory').addEventListener('click', () => loadCandidates().catch(e => notice(e.message, true)));
$('refreshjobs').addEventListener('click', () => loadJobs().catch(e => notice(e.message, true)));
$('refreshprocesses').addEventListener('click', () => loadProcesses().catch(e => notice(e.message, true)));
$('refreshgit').addEventListener('click', () => loadGitState().catch(e => notice(e.message, true)));
for (const tab of document.querySelectorAll('[data-view]')) tab.addEventListener('click', async () => {
  for (const t of document.querySelectorAll('[data-view]')) { const active = t === tab; t.classList.toggle('active', active); t.setAttribute('aria-pressed', String(active)); $(`view-${t.dataset.view}`).hidden = !active; }
  try { if (tab.dataset.view === 'memory') await loadCandidates(); if (tab.dataset.view === 'imports') { await loadJobs(); await loadProcesses(); await loadGitState(); } if (tab.dataset.view === 'settings') { const [data] = await Promise.all([api('/config'), loadProviders()]); $('mainmodel').value = data.main || ''; $('extractmodel').value = data.extraction || ''; $('verificationmodel').value = data.verification || ''; } } catch (error) { notice(error.message, true); }
});
// P11-T05: the two always-on clocks are now one tick, installed at the end of this file.

// P1-T13: the agent activity view reads the recorded rows; the durable receipt remains the
// source of truth, while these read-only endpoints make the current turn visible between model
// calls. No model/tool text is inserted as HTML.
const agentState = { requestId: null, sessionId: null, scope: null, permission: null, busyDecision: false, steps: [], changes: [], verification: null, incident: null, incidentNode: null };
function renderProjectOverview(config = scopeConfig || {}) {
  const currentScope = agentState.scope || scope;
  const hasRoot = typeof config.root_path === 'string' && config.root_path.length > 0;
  const mode = { ask: 'Ask', auto_edit: 'Auto-edit', auto_all: 'Auto-all' }[config.permission_mode] || 'Ask';
  $('project-overview-scope').textContent = currentScope;
  $('project-overview-tools').textContent = hasRoot ? 'Enabled' : 'Chat only';
  $('project-overview-mode').textContent = mode;
  $('project-overview-root').textContent = hasRoot ? config.root_path : 'No project root · file and command tools are off';
}


// P2-T01: the rail's transport is now the SSE feed instead of a 1 s clock. A frame only says
// that a row was committed — the rail still re-reads the durable endpoints — so the socket can
// never show something the database does not hold. `cursor` is the DB sequence, so a dropped
// stream is not a lost update: the reconnect resumes from the last `id` this tab received.
// Polling stays as the fallback for a browser or proxy that cannot hold a stream open.
const activityStream = { controller: null, sessionId: null, cursor: 0, live: false, failures: 0, retry: null, stale: false };
let railRefresh = null;
function requestRailRefresh() {
  // A hidden tab only records that something changed and re-reads once it is visible again.
  if (document.hidden) { activityStream.stale = true; return; }
  if (railRefresh) return;
  railRefresh = setTimeout(() => { railRefresh = null; activityStream.stale = false; refreshAgentTurn().catch(() => {}); }, 150);
}
document.addEventListener('visibilitychange', () => { if (!document.hidden && activityStream.stale) requestRailRefresh(); });
function closeActivityStream() {
  if (activityStream.retry) clearTimeout(activityStream.retry);
  if (activityStream.controller) activityStream.controller.abort();
  Object.assign(activityStream, { controller: null, sessionId: null, cursor: 0, live: false, failures: 0, retry: null, stale: false });
}
function consumeActivityFrame(frame) {
  // A comment frame (`: heartbeat`) is the socket proving it is alive; it is not an event.
  if (!frame.trim() || frame.startsWith(':')) return;
  let seq = null, kind = '';
  for (const line of frame.split('\n')) {
    if (line.startsWith('id:')) seq = Number(line.slice(3).trim());
    else if (line.startsWith('event:')) kind = line.slice(6).trim();
  }
  if (seq !== null && Number.isFinite(seq) && seq > activityStream.cursor) activityStream.cursor = seq;
  if (kind) requestRailRefresh();
}
async function followActivityStream(sessionId) {
  if (!token || !sessionId || activityStream.controller) return;
  if (activityStream.sessionId !== sessionId) { activityStream.sessionId = sessionId; activityStream.cursor = 0; }
  const controller = new AbortController(); const myEpoch = epoch;
  activityStream.controller = controller;
  try {
    // `fetch` and not `EventSource`: the bearer token belongs in a header, never in a URL.
    const response = await fetch(`/activity/stream?session_id=${encodeURIComponent(sessionId)}&after_seq=${activityStream.cursor}`,
      { headers: { 'Authorization': `Bearer ${token}` }, signal: controller.signal });
    if (!response.ok || !response.body) throw new Error(`Activity stream unavailable (${response.status})`);
    activityStream.live = true; activityStream.failures = 0; requestRailRefresh();
    const reader = response.body.getReader(); const decoder = new TextDecoder(); let buffer = '';
    for (;;) {
      const chunk = await reader.read();
      if (chunk.done) break;
      if (!token || myEpoch !== epoch) return;
      buffer += decoder.decode(chunk.value, { stream: true });
      // Frames end at a blank line; an incomplete frame waits in the buffer for its rest.
      const frames = buffer.split('\n\n'); buffer = frames.pop();
      for (const frame of frames) consumeActivityFrame(frame);
    }
  } catch (error) { if (error?.name !== 'AbortError') activityStream.failures++; }
  finally {
    if (activityStream.controller === controller) { activityStream.controller = null; activityStream.live = false; }
    controller.abort();
  }
  // Reconnect only while this tab still owns the turn, and stop after a few refusals so a
  // server or proxy without the endpoint falls back to polling instead of looping.
  if (token && myEpoch === epoch && pending?.session_id === sessionId && activityStream.failures < 3 && !activityStream.controller) {
    activityStream.retry = setTimeout(() => { activityStream.retry = null; followActivityStream(sessionId); }, Math.min(1000 * 2 ** activityStream.failures, 5000));
  }
}

function agentIcon(status) {
  if (status === 'complete') return '●';
  if (status === 'failed') return '✖';
  if (status === 'denied' || status === 'interrupted') return '⚠';
  return '○';
}

function agentMeta(step) {
  if (step.status === 'running') {
    // Elapsed time from the recorded start; the 1 s turn poll re-renders it.
    const started = new Date(step.started_at);
    if (!Number.isNaN(started.getTime())) {
      const secs = Math.max(0, Math.round((Date.now() - started.getTime()) / 1000));
      return `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, '0')}`;
    }
    return 'running';
  }
  if (step.status === 'queued') return 'queued';
  if (step.finished_at) {
    const date = new Date(step.finished_at);
    if (!Number.isNaN(date.getTime())) return date.toLocaleTimeString([], {hour:'2-digit', minute:'2-digit'});
  }
  return step.status || '';
}

// P5-T01: the verifier is advisory, so the badge only ever reports what the persisted
// verification step says. Claim and reason text comes from a model reading tool output, so it
// is placed with textContent (via `node`) and a title attribute — never parsed as markup.
function renderVerification(verification) {
  const badge = $('verification-badge');
  badge.replaceChildren();
  badge.className = 'badge verification-badge';
  badge.removeAttribute('title');
  badge.hidden = true;
  if (!verification || typeof verification !== 'object') return;
  const status = ['verified', 'unverified', 'skipped', 'unavailable'].includes(verification.status) ? verification.status : 'unavailable';
  const count = Number.isFinite(Number(verification.unverified_claims)) ? Number(verification.unverified_claims) : 0;
  const labels = {verified:'Verified', unverified:`${count} unverified`, skipped:'Verification skipped', unavailable:'Verification unavailable'};
  badge.classList.add(status);
  badge.append(node('span', labels[status]));
  const details = (Array.isArray(verification.claims) ? verification.claims : [])
    .filter(claim => claim && claim.status === 'unverified')
    .map(claim => `${String(claim.claim ?? 'Claim')} — ${String(claim.reason ?? 'no supporting evidence in this turn')}`);
  if (Array.isArray(verification.skipped_diagnostics)) details.push(...verification.skipped_diagnostics.map(item => String(item)));
  if (details.length) badge.title = details.join('\n');
  badge.hidden = false;
}

function renderAgentSteps(steps) {
  const panel = $('agent-steps');
  const list = $('steps-list');
  list.replaceChildren();
  if (!steps?.length) { panel.hidden = true; return; }
  panel.hidden = false;
  $('steps-count').textContent = `(${steps.length})`;
  for (const step of steps) {
    const detail = node('details', undefined, `agent-step ${step.status || ''}`);
    detail.dataset.stepId = step.id || '';
    const heading = node('summary');
    heading.append(node('span', agentIcon(step.status), 'step-icon'));
    heading.append(node('strong', step.tool_name || step.kind || 'step', 'step-tool'));
    heading.append(node('span', step.summary || (step.status === 'running' ? 'working…' : 'step recorded'), 'step-summary'));
    heading.append(node('span', agentMeta(step), 'step-meta'));
    const body = node('div', undefined, 'step-details');
    body.append(node('div', undefined, 'step-preview'));
    body.firstChild.append(node('strong', 'Input'), node('pre', step.input_preview || '(none)'));
    const output = node('div', undefined, 'step-preview');
    output.append(node('strong', 'Output'), node('pre', step.output_preview || '(none)'));
    body.append(output);
    if (step.previews_capped) body.append(node('p', 'Preview capped at 2 KB.', 'muted'));
    if (step.truncated) body.append(node('p', 'Tool output was capped.', 'muted'));
    detail.append(heading, body);
    list.append(detail);
  }
}

// P15-T02: every number here is read from the turn's persisted retrieval receipt. Keys and
// reasons come from stored memory text, so all of it is placed with textContent (via `node`)
// and never parsed as markup. The panel says what retrieval did; it makes no claim about how
// the answer used it.
const RETRIEVAL_REASONS = {
  ranked_and_fit: 'sent to the model',
  rank_cutoff: 'ranked below the top 20',
  payload_ceiling: 'would not fit the recall byte ceiling',
  category_budget: 'dropped by the context budget',
};
function renderRetrieval(receipt) {
  const panel = $('agent-retrieval');
  const list = $('retrieval-list');
  list.replaceChildren();
  $('retrieval-note').textContent = '';
  const rows = Array.isArray(receipt?.candidates) ? receipt.candidates : [];
  if (!rows.length) { panel.hidden = true; return; }
  panel.hidden = false;
  const included = rows.filter(row => row.decision === 'included').length;
  $('retrieval-count').textContent = `${included} of ${rows.length} sent`;
  $('retrieval-note').textContent = `${String(receipt.strategy || 'unknown')} retrieval · ${Number(receipt.included_bytes || 0)} of ${Number(receipt.budget_bytes || 0)} bytes used · recorded before the model answered`;
  for (const row of rows) {
    const included = row.decision === 'included';
    const item = node('div', undefined, `retrieval-row ${included ? 'included' : 'excluded'}`);
    item.append(node('span', `${String(row.key ?? 'memory')} · r${Number(row.revision || 0)}`, 'retrieval-key'));
    item.append(node('span', `score ${Number(row.total_score || 0).toFixed(3)}`, 'muted small'));
    const why = RETRIEVAL_REASONS[row.reason] || String(row.reason ?? 'recorded');
    item.append(node('span', `${included ? 'Included' : 'Excluded'} — ${why} · lexical ${Number(row.lexical_score || 0).toFixed(2)} · semantic ${Number(row.semantic_score || 0).toFixed(2)} · ${Number(row.bytes || 0)} bytes`, 'retrieval-why muted small'));
    list.append(item);
  }
}

function renderAgentPlan(plan) {
  const panel = $('agent-plan');
  const list = $('plan-items');
  list.replaceChildren();
  if (!plan?.items?.length) { panel.hidden = true; return; }
  panel.hidden = false;
  const done = plan.items.filter(item => item.status === 'done').length;
  $('plan-count').textContent = `${done}/${plan.items.length} done`;
  for (const item of plan.items) {
    const mark = item.status === 'done' ? '☑' : item.status === 'in_progress' ? '▶' : item.status === 'failed' ? '✖' : '☐';
    list.append(node('div', `${mark} ${item.text}`, `plan-item ${item.status || ''}`));
  }
}

function permissionText(permission) {
  if (!permission) return '';
  const args = permission.args;
  if (args && typeof args === 'object' && typeof args.diff === 'string') return args.diff;
  if (args && typeof args === 'object' && typeof args.command === 'string') return args.command;
  return JSON.stringify(args || {}, null, 2);
}

function renderAgentPermission(permission) {
  const card = $('agent-permission');
  if (!permission) {
    card.hidden = true;
    agentState.permission = null;
    return;
  }
  card.hidden = false;
  agentState.permission = permission;
  const bundle = Number(permission.bundle_count || 1);
  $('permission-summary').textContent = `Approval requested for ${permission.tool}: ${permission.summary}. Nothing will run until you approve.`;
  const expiry = Date.parse(permission.expires_at || '');
  const remaining = Number.isFinite(expiry) ? Math.max(0, Math.ceil((expiry - Date.now()) / 1000)) : null;
  $('permission-expiry').textContent = `${bundle > 1 ? `${bundle} approvals are grouped for this turn. ` : ''}${remaining === null ? 'Approval deadline unavailable.' : remaining ? `Expires in ${remaining}s.` : 'Approval expired; refresh to confirm the recorded state.'}`;
  $('permission-detail').textContent = permissionText(permission);
  $('permission-approve').disabled = agentState.busyDecision;
  $('permission-deny').disabled = agentState.busyDecision;
}

function agentStatusLabel(state) {
  return {captured:'Saved · queued', generating:'Thinking…', complete:'Done', failed:'Saved · answer failed', interrupted:'Saved · interrupted'}[state] || 'Saved';
}

// P2-T03: diff cards. A diff is tool text, so each line becomes a <span> built from textContent
// and coloured by class — CSP forbids inline styles, and none of this is ever parsed as HTML.
function diffCounts(diff) {
  let plus = 0, minus = 0;
  for (const line of String(diff || '').split('\n')) {
    if (line.startsWith('+++') || line.startsWith('---')) continue;
    if (line.startsWith('+')) plus++;
    else if (line.startsWith('-')) minus++;
  }
  return { plus, minus };
}
function diffLineClass(line) {
  if (line.startsWith('@@') || line.startsWith('+++') || line.startsWith('---') || line.startsWith('\\')) return 'meta';
  if (line.startsWith('+')) return 'add';
  if (line.startsWith('-')) return 'del';
  return '';
}
// The row's own word: never say "applied" for a change the record does not call applied.
function changeWhen(change) {
  const label = change.reverted_at ? 'reverted' : change.applied ? 'applied' : 'recorded';
  const at = new Date(change.reverted_at || change.created_at);
  if (Number.isNaN(at.getTime())) return label;
  return `${label} ${at.toLocaleTimeString([], {hour:'2-digit', minute:'2-digit'})}`;
}
function renderAgentChanges(changes) {
  const panel = $('agent-changes');
  const list = $('changes-list');
  list.replaceChildren();
  if (!changes?.length) { panel.hidden = true; return; }
  panel.hidden = false;
  $('changes-count').textContent = `(${changes.length})`;
  for (const change of changes) {
    const card = node('div', undefined, 'diff-card');
    card.dataset.changeId = change.id || '';
    const head = node('div', undefined, 'diff-head');
    const counts = diffCounts(change.diff);
    const stat = node('span', undefined, 'diff-stat');
    stat.append(node('span', `+${counts.plus}`, 'plus'), node('span', ` −${counts.minus}`, 'minus'));
    head.append(node('span', change.path || 'file', 'diff-path'), stat, node('span', changeWhen(change), 'diff-when'));
    const body = node('pre', undefined, 'diff-body');
    const text = String(change.diff || '');
    if (text) for (const line of text.split('\n')) body.append(node('span', `${line}\n`, `diff-line ${diffLineClass(line)}`));
    else body.append(node('span', 'No diff was recorded for this change.', 'diff-line meta'));
    const foot = node('div', undefined, 'diff-foot');
    if (change.revertable) {
      const button = node('button', 'Revert');
      button.type = 'button';
      button.dataset.changeId = change.id;
      button.addEventListener('click', () => revertChange(change.id, button));
      foot.append(node('span', 'Restores the content recorded before this edit.', 'muted'), button);
    } else {
      // The server already decided this, by reading the file: say why instead of offering an
      // undo that would refuse.
      foot.append(node('span', change.revert_note || 'Revert unavailable', 'muted'));
    }
    card.append(head, body, foot);
    list.append(card);
  }
}
async function revertChange(id, button) {
  button.disabled = true;
  try {
    const result = await api(`/changes/${encodeURIComponent(id)}/revert`, {});
    notice(result.status === 'deleted' ? 'Reverted. The file this turn created was removed.' : 'Reverted. The file is back to the content recorded before the edit.');
    await refreshAgentTurn({request_id: agentState.requestId, session_id: agentState.sessionId, scope: agentState.scope});
  } catch (error) {
    notice(error.message, true);
    button.disabled = false;
  }
}

const incidentRelations = ['supports','contradicts','depends_on','authorizes','mutates','invalidates','triggers'];
const incidentKinds = ['request','step','permission','mutation','evidence','memory','recovery'];
// P16-T01: what the reviewer has asked the *server* for. Filters are server-side because the
// server holds the whole recorded set and this client only ever holds a bounded page of it —
// filtering the page in the browser would silently search less than the reviewer thinks.
const incidentQuery = { view: 'causal', relation: 'all', kind: 'all', status: '', q: '', anchor: null };
function incidentNode(graph, id) { return (graph?.nodes || []).find(item => item.id === id); }
function focusDurableRow(item) {
  const selector = item.kind === 'step' ? `.agent-step[data-step-id="${CSS.escape(item.row_id)}"]` : item.kind === 'mutation' ? `.diff-card[data-change-id="${CSS.escape(item.row_id)}"]` : '';
  const target = selector && document.querySelector(selector);
  if (!target) return notice('This durable row is identified in the graph but has no separate rail view.');
  if (target.tagName === 'DETAILS') target.open = true;
  target.scrollIntoView({block:'center',behavior:'smooth'}); target.focus?.({preventScroll:true});
}
// The label a confidence value is shown under. The server also ships a legend; these strings
// exist so the rail never renders a bare identifier, and they deliberately do not promote
// proximity into a causal claim.
const CONFIDENCE_LABELS = Object.freeze({
  recorded_dependency: 'recorded dependency',
  temporal_proximity: 'temporal proximity only',
  unknown: 'unknown',
});
function confidenceLabel(value) { return CONFIDENCE_LABELS[value] || 'unknown'; }
function renderIncidentDetail(graph, item) {
  const detail = $('incident-detail'); detail.replaceChildren(); detail.hidden = !item;
  if (!item) return;
  detail.append(node('span', item.kind, `incident-kind ${item.kind}`), node('strong', item.label || item.id));
  detail.append(node('p', `${item.status || 'recorded'} · ${confidenceLabel(item.confidence)}`, `muted incident-confidence-line ${item.confidence || 'unknown'}`));
  if (item.confidence === 'temporal_proximity') detail.append(node('p', 'This row occurred in the same request and has a recorded time, but no dependency was recorded. Co-occurrence is not causation.', 'muted small'));
  if (item.confidence === 'unknown') detail.append(node('p', 'No dependency and no usable recorded time. Nothing is claimed about this row.', 'muted small'));
  if (item.at) detail.append(node('p', `Recorded at ${item.at}`, 'muted small'));
  const relations = (graph.edges || []).filter(edge => edge.source === item.id || edge.target === item.id);
  const list = node('ul', undefined, 'incident-links');
  for (const edge of relations) {
    const outward = edge.source === item.id; const other = incidentNode(graph, outward ? edge.target : edge.source);
    const button = node('button', `${outward ? '→' : '←'} ${edge.relation} · ${other?.label || (outward ? edge.target : edge.source)}`, 'incident-link'); button.type = 'button';
    button.addEventListener('click', () => { agentState.incidentNode = other?.id || null; renderIncident(graph); }); list.append(button);
  }
  if (relations.length) detail.append(list); else detail.append(node('p', 'No recorded edge. This gap is explicit, not inferred.', 'muted'));
  if (item.kind === 'step' || item.kind === 'mutation') { const back = node('button', item.kind === 'step' ? 'Show recorded step' : 'Show file change', 'secondary'); back.type = 'button'; back.addEventListener('click', () => focusDurableRow(item)); detail.append(back); }
  detail.append(node('code', item.row_id, 'incident-row-id'));
}
function renderIncidentTimeline(graph) {
  const target = $('incident-timeline'); target.replaceChildren();
  const chronological = incidentQuery.view === 'chronological';
  target.hidden = !chronological;
  $('incident-nodes').hidden = chronological;
  if (!chronological) return;
  const timeline = graph.timeline || [];
  if (!timeline.length) return target.append(node('p', 'No recorded rows in this view.', 'muted'));
  for (const entry of timeline) {
    const row = node('button', undefined, `incident-timeline-row${agentState.incidentNode === entry.node_id ? ' active' : ''}`);
    row.type = 'button'; row.dataset.confidence = entry.confidence || 'unknown';
    row.append(node('span', entry.at || 'no recorded time', 'incident-time'));
    row.append(node('span', entry.kind, `incident-kind ${entry.kind}`));
    row.append(node('span', entry.label || entry.node_id, 'incident-label'));
    row.append(node('span', confidenceLabel(entry.confidence), `incident-confidence-tag ${entry.confidence || 'unknown'}`));
    row.addEventListener('click', () => { agentState.incidentNode = entry.node_id; renderIncident(graph); });
    target.append(row);
  }
  if (graph.timeline_undated) target.append(node('p', `${graph.timeline_undated} row(s) carry no recorded time and are listed last rather than placed by guesswork.`, 'muted small'));
}
function renderIncident(graph) {
  const panel = $('agent-incident'), select = $('incident-relation'), kindSelect = $('incident-kind'), list = $('incident-nodes'); list.replaceChildren();
  if (!graph?.nodes?.length) {
    // A filter that matched only the request frame is a real answer, not an empty panel: keep
    // the panel up so the reviewer can widen the filter again.
    if (!graph || !incidentQueryIsFiltered()) { panel.hidden = true; agentState.incident = null; return; }
  }
  panel.hidden = false; agentState.incident = graph;
  if (select.options.length === 1) for (const relation of incidentRelations) select.append(new Option(relation.replace('_',' '), relation));
  if (kindSelect.options.length === 1) for (const kind of incidentKinds) kindSelect.append(new Option(kind, kind));
  const nodes = graph.nodes || [], edges = graph.edges || [];
  const counts = graph.counts || {};
  $('incident-count').textContent = `${nodes.length} of ${counts.nodes?.total ?? nodes.length} nodes · ${edges.length} of ${counts.edges?.total ?? edges.length} edges`;
  const confidence = counts.confidence || {};
  $('incident-confidence').textContent = `${confidence.recorded_dependency || 0} recorded dependency · ${confidence.temporal_proximity || 0} temporal proximity only · ${confidence.unknown || 0} unknown`;
  const earliest = graph.earliest_known_break || {};
  $('incident-break').textContent = earliest.known ? `Earliest known break: ${earliest.reason}` : 'Earliest break unknown';
  // Bounded expansion. The cursor is passed back verbatim; this client never builds one.
  const cursor = graph.expansion_cursors?.nodes || null;
  const expand = $('incident-expand');
  expand.hidden = !cursor;
  if (cursor) {
    expand.textContent = `Expand ${counts.nodes?.omitted || 0} more recorded node(s)`;
    expand.onclick = () => { incidentQuery.anchor = cursor; void reloadIncident(); };
  }
  if (!nodes.length) list.append(node('p', 'No recorded row matches this filter. The filter narrowed the recorded set; it did not conclude anything.', 'muted'));
  for (const item of nodes) {
    const button = node('button', undefined, `incident-node${agentState.incidentNode === item.id ? ' active' : ''}`); button.type = 'button';
    button.dataset.confidence = item.confidence || 'unknown';
    button.append(node('span', item.kind, `incident-kind ${item.kind}`), node('span', item.label || item.id, 'incident-label'), node('span', item.status || 'recorded', 'incident-status'), node('span', confidenceLabel(item.confidence), `incident-confidence-tag ${item.confidence || 'unknown'}`));
    button.addEventListener('click', () => { agentState.incidentNode = item.id; renderIncident(graph); }); list.append(button);
  }
  renderIncidentTimeline(graph);
  const selected = incidentNode(graph, agentState.incidentNode) || incidentNode(graph, earliest.node_id) || nodes[0];
  agentState.incidentNode = selected?.id || null; renderIncidentDetail(graph, selected);
}
function incidentQueryIsFiltered() {
  return incidentQuery.relation !== 'all' || incidentQuery.kind !== 'all' || Boolean(incidentQuery.status) || Boolean(incidentQuery.q);
}
function incidentQueryParams() {
  const params = new URLSearchParams({ view: incidentQuery.view });
  if (incidentQuery.relation !== 'all') params.set('relation', incidentQuery.relation);
  if (incidentQuery.kind !== 'all') params.set('kind', incidentQuery.kind);
  if (incidentQuery.status) params.set('status', incidentQuery.status);
  if (incidentQuery.q) params.set('q', incidentQuery.q);
  if (incidentQuery.anchor) params.set('anchor', JSON.stringify(incidentQuery.anchor));
  return params;
}
// Re-ask the server. Every filter change goes through here, so the rail can never show a
// locally-narrowed view that the reviewer mistakes for the whole recorded set.
async function reloadIncident() {
  if (!agentState.requestId) return;
  try {
    const graph = await api(`/chat/requests/${encodeURIComponent(agentState.requestId)}/incident?${incidentQueryParams().toString()}`);
    renderIncident(graph);
  } catch (error) { notice(error.message, true); }
}
function setIncidentView(view) {
  incidentQuery.view = view; incidentQuery.anchor = null;
  for (const [id, value] of [['incident-view-causal','causal'],['incident-view-chronological','chronological']]) {
    const button = $(id); const active = value === view;
    button.classList.toggle('active', active); button.setAttribute('aria-pressed', String(active));
  }
  void reloadIncident();
}
$('incident-view-causal').addEventListener('click', () => setIncidentView('causal'));
$('incident-view-chronological').addEventListener('click', () => setIncidentView('chronological'));
$('incident-relation').addEventListener('change', event => { incidentQuery.relation = event.target.value; incidentQuery.anchor = null; void reloadIncident(); });
$('incident-kind').addEventListener('change', event => { incidentQuery.kind = event.target.value; incidentQuery.anchor = null; void reloadIncident(); });
$('incident-search').addEventListener('change', event => { incidentQuery.q = event.target.value.trim(); incidentQuery.anchor = null; void reloadIncident(); });
$('incident-status').addEventListener('change', event => { incidentQuery.status = event.target.value.trim(); incidentQuery.anchor = null; void reloadIncident(); });

async function refreshAgentTurn(receipt) {
  const requestId = receipt?.request_id || pending?.request_id;
  const sessionId = receipt?.session_id || pending?.session_id || session;
  const turnScope = receipt?.scope || pending?.scope || scope;
  if (!token || !requestId) return;
  const data = await Promise.all([
    api(`/chat/requests/${encodeURIComponent(requestId)}/steps`),
    api(`/sessions/${encodeURIComponent(sessionId)}/plan`),
    api(`/permissions?scope=${encodeURIComponent(turnScope)}`),
    // A turn with no recorded changes is normal, and a server without this route is not a
    // reason to blank the rest of the rail.
    api(`/changes?request_id=${encodeURIComponent(requestId)}`).catch(() => ({changes: []})),
    api(`/chat/requests/${encodeURIComponent(requestId)}/incident`).catch(() => null),
    // A turn recorded before this route existed simply has no retrieval receipt; that is not a
    // reason to blank the rest of the rail.
    api(`/chat/requests/${encodeURIComponent(requestId)}/retrieval`).catch(() => null),
  ]);
  if (!token || requestId !== (pending?.request_id || requestId) || sessionId !== session) return;
  agentState.requestId = requestId; agentState.sessionId = sessionId; agentState.scope = turnScope;
  renderProjectOverview();
  $('agent-turn').hidden = false;
  // P2-T02: the turn record lives in the right rail; auto-open it on wide screens only.
  if (!window.matchMedia('(max-width: 1100px)').matches) { $('rail').hidden = false; $('railbtn').setAttribute('aria-expanded', 'true'); }
  $('agent-turn-status').textContent = agentStatusLabel(receipt?.state);
  agentState.verification = data[0].verification || null;
  renderVerification(agentState.verification);
  agentState.steps = data[0].steps || [];
  renderAgentSteps(agentState.steps);
  renderAgentPlan(data[1]);
  agentState.changes = data[3].changes || [];
  renderAgentChanges(agentState.changes);
  renderIncident(data[4]);
  renderRetrieval(data[5]);
  // P2 context meter placeholder: real tokens-so-far from step receipts; the budget bar lands in P3.
  const tokens = (data[0].steps || []).reduce((sum, s) => sum + (s.tokens_in || 0) + (s.tokens_out || 0), 0);
  $('context-tokens').textContent = tokens ? `${tokens.toLocaleString()} tokens so far` : 'No recorded usage yet';
  $('usage-tokens').textContent = tokens ? `${tokens.toLocaleString()} tokens` : 'No usage yet';
  $('usage-cost').textContent = 'Tokens only · no price configured';
  const match = (data[2].permissions || []).find(item => item.request_id === requestId);
  renderAgentPermission(match || null);
  if (!match && !agentState.steps.length && !(data[1].items || []).length && !agentState.changes.length && !agentState.verification) { $('agent-turn').hidden = true; $('rail').hidden = true; }
}

async function decideAgentPermission(decision) {
  const permission = agentState.permission;
  if (!permission || agentState.busyDecision) return;
  agentState.busyDecision = true;
  renderAgentPermission(permission);
  try {
    await api(`/permissions/${encodeURIComponent(permission.id)}`, { decision, scope: agentState.scope || scope });
    renderAgentPermission(null);
    notice(decision === 'approve' ? 'Approval recorded. The agent can continue.' : 'Denied. The agent will record the refusal.');
    await refreshAgentTurn({request_id: agentState.requestId, session_id: agentState.sessionId, scope: agentState.scope});
  } catch (error) {
    notice(error.message, true);
  } finally {
    agentState.busyDecision = false;
    if (agentState.permission) renderAgentPermission(agentState.permission);
  }
}

$('permission-approve').addEventListener('click', () => decideAgentPermission('approve'));
$('permission-deny').addEventListener('click', () => decideAgentPermission('deny'));

// P11-T05: one clock for the whole tab. Before this there were two unconditional timers (1 s
// for the running turn, 5 s for status and inline suggestions) that kept firing while the tab
// was hidden and only then checked `document.hidden`, so a backgrounded tab still woke the
// event loop twice a second forever. Now a hidden tab runs no timer at all: it is stopped on
// `visibilitychange`, and becoming visible does one immediate catch-up tick before restarting.
// The work itself is unchanged — while the stream is live the turn only re-renders elapsed time
// from rows already fetched; with no stream it is still the P1-T13 poll — so this changes when
// idle work runs, never what it reads or displays.
const idleClock = { timer: null, ticks: 0 };
const IDLE_TICK_MS = 1000, STATUS_EVERY_TICKS = 5;
function idleTick() {
  if (!token) return;
  idleClock.ticks++;
  if (pending) {
    if (activityStream.live) { if (agentState.steps.length) renderAgentSteps(agentState.steps); }
    else refreshAgentTurn().catch(() => {});
  }
  if (idleClock.ticks % STATUS_EVERY_TICKS === 0) { refreshStatus().catch(() => {}); refreshInlineSuggestions().catch(() => {}); }
}
function startIdleClock() {
  if (idleClock.timer || document.hidden) return;
  idleClock.timer = setInterval(idleTick, IDLE_TICK_MS);
}
function stopIdleClock() {
  if (!idleClock.timer) return;
  clearInterval(idleClock.timer); idleClock.timer = null;
}
document.addEventListener('visibilitychange', () => {
  if (document.hidden) { stopIdleClock(); return; }
  // Coming back visible: re-read once immediately so the view is not a tick behind.
  idleClock.ticks = STATUS_EVERY_TICKS - 1; idleTick(); startIdleClock();
});
startIdleClock();

function fillProjectSettings(data) {
  $('rootpath').value = data.root_path || '';
  $('permissionmode').value = data.permission_mode || 'ask';
  $('diagnosticscmd').value = data.diagnostics_cmd || '';
  $('maxsteps').value = data.max_steps ?? 40;
  $('maxtoolbytes').value = data.max_tool_bytes ?? 400000;
  $('maxwallseconds').value = data.max_wall_seconds ?? 900;
}

async function loadProjectSettings() {
  try {
    fillProjectSettings(await api(`/scopes/${encodeURIComponent(scope)}`));
  } catch (error) {
    if (error.status === 404) fillProjectSettings({permission_mode:'ask', max_steps:40, max_tool_bytes:400000, max_wall_seconds:900});
    else throw error;
  }
}

$('projectform').addEventListener('submit', async event => {
  event.preventDefault();
  const numberOrNull = id => $(id).value === '' ? null : Number($(id).value);
  const payload = {
    root_path: $('rootpath').value.trim() || null,
    permission_mode: $('permissionmode').value,
    diagnostics_cmd: $('diagnosticscmd').value.trim() || null,
    max_steps: numberOrNull('maxsteps'),
    max_tool_bytes: numberOrNull('maxtoolbytes'),
    max_wall_seconds: numberOrNull('maxwallseconds'),
  };
  try {
    await api(`/scopes/${encodeURIComponent(scope)}`, payload);
    notice(`Project settings saved for ${scope}.`);
    await refreshScopeSetup();
  } catch (error) { notice(error.message, true); }
});

for (const tab of document.querySelectorAll('[data-view]')) tab.addEventListener('click', () => {
  if (tab.dataset.view === 'settings' && token) loadProjectSettings().catch(error => notice(error.message, true));
});

// P1-T15 first run. Tools exist only for a scope with a project root, so the chat view has to say
// that before the model does: a tool-less turn used to come back as "I have no terminal in this
// conversation", which reads as a broken product rather than one unset field.
async function refreshScopeSetup() {
  if (!token) return;
  const myEpoch = epoch;
  const data = await api('/scopes');
  if (!token || myEpoch !== epoch) return;
  const scopes = Array.isArray(data.scopes) ? data.scopes : [];
  const options = document.createDocumentFragment();
  for (const item of scopes) {
    const option = document.createElement('option');
    option.value = String(item.scope);
    option.label = item.root_path ? String(item.root_path) : 'no project root · tools off';
    options.append(option);
  }
  $('scopelist').replaceChildren(options);
  const current = scopes.find(item => item.scope === scope);
  $('setup-banner').hidden = !!current?.root_path;
  const ready = scopes.filter(item => item.root_path).map(item => item.scope);
  $('setup-banner-text').textContent = `Scope “${scope}” has no project root, so the file and command tools are not attached and the agent can only talk. Set a root path to turn them on.`
    + (ready.length ? ` Scopes already set up: ${ready.join(', ')}.` : '');
}

$('setup-open').addEventListener('click', () => {
  const tab = document.querySelector('[data-view="settings"]');
  if (tab) tab.click();
  $('rootpath').focus();
});
$('scope').addEventListener('change', () => refreshScopeSetup().catch(() => {}));

// P2-T02 shell: theme toggle, mobile navigation drawer, activity rail, Enter-to-send.
// CSP is style-src 'self', so presentation state moves via classes, the hidden property
// and data-theme on <html> — never inline style attributes. The theme choice is not
// sensitive, so it may persist in localStorage; tokens and drafts still never persist.
try {
  const savedTheme = localStorage.getItem('harness_theme');
  if (savedTheme === 'light' || savedTheme === 'dark') document.documentElement.dataset.theme = savedTheme;
} catch { /* storage unavailable; default theme stays */ }
$('themebtn').addEventListener('click', () => {
  const next = document.documentElement.dataset.theme === 'light' ? 'dark' : 'light';
  document.documentElement.dataset.theme = next;
  try { localStorage.setItem('harness_theme', next); } catch { /* ignore */ }
});
function setDrawer(open, restoreFocus = false) {
  $('sidebar').classList.toggle('open', open);
  $('drawerbg').classList.toggle('show', open);
  $('navtoggle').setAttribute('aria-expanded', String(open));
  if (open) $('sidebar').querySelector('input,button,summary')?.focus();
  else if (restoreFocus) $('navtoggle').focus();
}
$('navtoggle').addEventListener('click', () => setDrawer(!$('sidebar').classList.contains('open')));
$('drawerbg').addEventListener('click', () => setDrawer(false, true));
$('railbtn').addEventListener('click', () => {
  const open = $('rail').hidden;
  $('rail').hidden = !open;
  $('railbtn').setAttribute('aria-expanded', String(open));
  if (open) $('rail').querySelector('button,select')?.focus();
});
$('railclose').addEventListener('click', () => {
  $('rail').hidden = true;
  $('railbtn').setAttribute('aria-expanded', 'false');
  $('railbtn').focus();
});
document.addEventListener('keydown', event => {
  if (event.key !== 'Escape') return;
  if ($('sidebar').classList.contains('open')) { setDrawer(false, true); return; }
  if (!$('rail').hidden) { $('rail').hidden = true; $('railbtn').setAttribute('aria-expanded', 'false'); $('railbtn').focus(); }
});
// CLI-style composer: Enter sends, Shift+Enter keeps the newline (design: ui.md#keyboard).
$('prompt').addEventListener('keydown', event => {
  if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) { event.preventDefault(); $('chatform').requestSubmit(); }
});

// ---------------------------------------------------------------------------
// P15-T05: the reviewer surface for the P15-T04 backend.
//
// Three panels that must never read as one operation. Search returns citations, never a claim
// that an answer would have changed. Forget and delete-source are separate controls with
// separate copy and separate confirmation, because one is reversible suppression and the other
// destroys content. Export shows the exact item list and the digest it will be pinned to before
// any release control exists.
//
// No response text is ever parsed as markup: every server string reaches the DOM through
// `node()` / `textContent`, which is the same discipline the rest of this file holds.
// ---------------------------------------------------------------------------
const historyState = { results: [], selected: null, audit: null, bundle: null };

function citationLine(citation) {
  // A citation is shown field by field rather than as a sentence, so a reader can check the
  // exact revision and checksum a hit was served from.
  const list = node('dl', undefined, 'citation-grid');
  for (const [label, value] of [
    ['Kind', citation.kind],
    ['Source row', citation.source_id],
    ['Revision', citation.revision],
    ['Scope', citation.scope],
    ['Session', citation.session_id || 'not session-scoped'],
    ['Recorded', citation.timestamp],
    ['Checksum', citation.content_sha256],
    ['Sanitizer', citation.sanitizer],
  ]) {
    const row = node('div');
    row.append(node('dt', label), node('dd', value === undefined || value === null ? 'unknown' : String(value)));
    list.append(row);
  }
  return list;
}

function renderHistoryResults(payload) {
  const target = $('history-results');
  target.replaceChildren();
  const results = payload?.results || [];
  historyState.results = results;
  $('history-summary').textContent = payload
    ? `${payload.returned} of ${payload.hits} shown · ${payload.suppressed} suppressed by the read-time sanitizer gate · sanitizer ${payload.sanitizer}`
    : '';
  if (!results.length) {
    target.append(node('p', payload?.note || 'Nothing matched in this scope.', 'empty'));
    return;
  }
  for (const hit of results) {
    const card = node('article', undefined, 'history-hit');
    card.dataset.documentId = hit.citation.id;
    card.append(node('h3', hit.title || hit.citation.source_id));
    card.append(node('p', hit.snippet, 'history-snippet'));
    if (hit.snippet_truncated) card.append(node('p', 'Snippet truncated at the shared bound.', 'muted small'));
    const cite = node('details', undefined, 'history-citation');
    cite.append(node('summary', 'Citation'));
    cite.append(citationLine(hit.citation));
    card.append(cite);
    const select = node('button', 'Privacy controls', 'secondary');
    select.type = 'button';
    select.addEventListener('click', () => selectHistoryDocument(hit.citation.id));
    card.append(select);
    target.append(card);
  }
}

// The two operations, rendered as two visibly different controls. The copy states what each one
// does and does not do; neither button implies the other's effect.
function renderHistoryPrivacy() {
  const panel = $('history-privacy');
  panel.replaceChildren();
  const audit = historyState.audit;
  if (!audit) {
    panel.append(node('p', 'Select a search result to see its privacy controls.', 'empty'));
    $('history-audit').hidden = true;
    return;
  }
  panel.append(node('h3', audit.title || audit.citation?.id || historyState.selected));
  const forgotten = Boolean(audit.forgotten_at);
  const deleted = Boolean(audit.source_deleted_at);
  const state = node('p', undefined, 'history-state');
  state.append(node('span', deleted ? 'source deleted' : forgotten ? 'forgotten' : 'searchable', `history-badge ${deleted ? 'deleted' : forgotten ? 'forgotten' : 'live'}`));
  // The distinction that must be visible: content survives a forget and does not survive a
  // source delete. This reads the server's own recorded flag rather than inferring it.
  state.append(node('span', audit.content_present ? 'content retained' : 'content removed', 'muted small'));
  panel.append(state);
  if (audit.note) panel.append(node('p', audit.note, 'muted small'));

  const forgetBox = node('div', undefined, 'privacy-op reversible');
  forgetBox.append(node('h4', forgotten ? 'Restore' : 'Forget'));
  forgetBox.append(node('p', forgotten
    ? 'Lift the suppression. The content was never destroyed, which is why this is possible.'
    : 'Stop this entry being recalled or returned. Content, citation and revision trail are kept, and this is reversible.', 'muted small'));
  const forgetButton = node('button', forgotten ? 'Restore entry' : 'Forget entry', 'secondary');
  forgetButton.type = 'button';
  forgetButton.id = 'history-forget';
  forgetButton.disabled = deleted;
  forgetButton.addEventListener('click', () => runHistoryPrivacy(forgotten ? 'restore' : 'forget', forgetButton));
  forgetBox.append(forgetButton);
  if (deleted) forgetBox.append(node('p', 'This entry\u2019s source was deleted, so there is nothing left to forget.', 'muted small'));
  panel.append(forgetBox);

  const deleteBox = node('div', undefined, 'privacy-op destructive');
  deleteBox.append(node('h4', 'Delete source'));
  deleteBox.append(node('p', 'Remove the stored content. Only the audited fact that this entry existed and was deleted remains. This is a different operation from Forget and it cannot be undone.', 'muted small'));
  const deleteButton = node('button', 'Delete source content', 'danger');
  deleteButton.type = 'button';
  deleteButton.id = 'history-delete-source';
  deleteButton.disabled = deleted;
  deleteButton.addEventListener('click', () => runHistoryPrivacy('delete_source', deleteButton));
  deleteBox.append(deleteButton);
  if (deleted) deleteBox.append(node('p', 'Already deleted; nothing would be written twice.', 'muted small'));
  panel.append(deleteBox);

  const auditPanel = $('history-audit');
  auditPanel.replaceChildren();
  auditPanel.hidden = false;
  auditPanel.append(node('h3', 'Recorded privacy events'));
  const events = audit.events || [];
  if (!events.length) auditPanel.append(node('p', 'No privacy event recorded for this entry.', 'muted'));
  const list = node('ul', undefined, 'audit-list');
  for (const event of events) {
    list.append(node('li', `${event.created_at} · ${event.action} · revision ${event.revision} · ${event.detail || 'no detail recorded'}`));
  }
  if (events.length) auditPanel.append(list);
}

async function selectHistoryDocument(id) {
  historyState.selected = id;
  try {
    historyState.audit = await api(`/history/documents/${encodeURIComponent(id)}`);
  } catch (error) {
    historyState.audit = null;
    notice(error.message, true);
  }
  renderHistoryPrivacy();
}

async function runHistoryPrivacy(action, button) {
  // Deleting content is confirmed separately and by name, because it is not reversible and must
  // not be reachable by the same reflex as a forget.
  if (action === 'delete_source' && !window.confirm('Delete the stored content of this entry? Forget is reversible; this is not.')) return;
  button.disabled = true;
  try {
    const result = await api(`/history/documents/${encodeURIComponent(historyState.selected)}/privacy`, { action });
    notice(result.note || `Recorded: ${result.outcome}`);
    await selectHistoryDocument(historyState.selected);
  } catch (error) {
    notice(error.message, true);
    button.disabled = false;
  }
}

$('historysearchform').addEventListener('submit', async event => {
  event.preventDefault();
  const query = $('history-query').value.trim();
  if (!query) return;
  const params = new URLSearchParams({ q: query, scope });
  const kind = $('history-kind').value;
  if (kind) params.set('kind', kind);
  const sessionFilter = $('history-session').value.trim();
  if (sessionFilter) params.set('session_id', sessionFilter);
  $('history-search-button').disabled = true;
  try {
    renderHistoryResults(await api(`/history/search?${params.toString()}`));
  } catch (error) {
    notice(error.message, true);
  } finally {
    $('history-search-button').disabled = false;
  }
});

$('history-index').addEventListener('click', async () => {
  try {
    const result = await api('/history/index', {});
    notice(`Index refreshed: ${result.indexed} new, ${result.revision_advanced} advanced, ${result.unchanged} unchanged, ${(result.refused || []).length} refused by the sanitizer.`);
  } catch (error) {
    notice(error.message, true);
  }
});

// Export review. The item list and the digest that would pin it are shown before any control
// that releases anything exists, so "what will leave" is on screen first. The digest comes from
// the server's own preview (`review` with `approve: false`); this client never computes one.
function renderExportPreview(bundle) {
  const panel = $('export-preview');
  panel.replaceChildren();
  historyState.bundle = bundle;
  if (!bundle) { panel.hidden = true; return; }
  panel.hidden = false;
  const state = bundle.state || (bundle.outcome === 'reviewed' ? 'reviewed' : 'draft');
  panel.append(node('h3', state === 'released' ? 'This left the machine' : 'This is what would leave'));
  panel.append(node('p', `Bundle ${bundle.bundle_id} · state ${state} · audience ${bundle.audience || 'not recorded'}`, 'muted small'));
  const items = bundle.items || [];
  panel.append(node('p', `${items.length} item${items.length === 1 ? '' : 's'} · content digest ${bundle.content_sha256 || 'not computed yet'}`, 'export-digest'));
  if (bundle.unsanitized_items) panel.append(node('p', `${bundle.unsanitized_items} item(s) were refused by the shared sanitizer and cannot be reviewed or released.`, 'muted small'));
  const list = node('ul', undefined, 'export-items');
  for (const item of items) {
    const entry = node('li');
    entry.append(node('span', `${item.kind} · revision ${item.revision} · ${item.sanitized ? 'sanitized' : 'refused by the sanitizer'}`, 'export-item-kind'));
    entry.append(node('span', item.stable_id || item.id, 'export-item-title'));
    const body = item.payload?.title || item.payload?.key || item.payload?.body;
    if (body) entry.append(node('p', String(body), 'export-item-preview'));
    list.append(entry);
  }
  if (items.length) panel.append(list); else panel.append(node('p', 'The draft selected nothing, so there is nothing to release.', 'muted'));
  if (bundle.note) panel.append(node('p', bundle.note, 'muted small'));

  if (state === 'draft' && items.length && bundle.content_sha256) {
    const approve = node('button', 'Approve exactly these items for this audience', 'secondary');
    approve.type = 'button'; approve.id = 'export-approve';
    approve.addEventListener('click', async () => {
      approve.disabled = true;
      try {
        const reviewed = await api(`/export/bundles/${encodeURIComponent(bundle.bundle_id)}/review`, { approve: true, content_sha256: bundle.content_sha256 });
        notice('Review recorded against this exact digest. Release still has to be asked for separately.');
        renderExportPreview({ ...bundle, ...reviewed, state: 'reviewed', items: bundle.items, audience: bundle.audience });
      } catch (error) { notice(error.message, true); approve.disabled = false; }
    });
    panel.append(approve);
  }
  if (state === 'reviewed') {
    const release = node('button', 'Release the reviewed packet', 'danger');
    release.type = 'button'; release.id = 'export-release';
    release.addEventListener('click', async () => {
      if (!window.confirm('Release this reviewed packet? The listed sanitized items will leave this machine.')) return;
      release.disabled = true;
      try {
        await api(`/export/bundles/${encodeURIComponent(bundle.bundle_id)}/release`, {});
        notice('Released. The digest was recomputed and matched what was reviewed.');
        renderExportPreview({ ...bundle, state: 'released' });
      } catch (error) { notice(error.message, true); release.disabled = false; }
    });
    panel.append(release);
  }
  if (state === 'released') panel.append(node('p', 'Released. Nothing further leaves without a new draft and a new review.', 'muted small'));
}

$('exportform').addEventListener('submit', async event => {
  event.preventDefault();
  const audience = $('export-audience').value.trim();
  if (!audience || !$('export-consent').checked) return;
  const documentIds = historyState.results.map(hit => hit.citation.id);
  if (!documentIds.length) return notice('Search first: an export draft is assembled from the current search results.', true);
  $('export-draft').disabled = true;
  try {
    const draft = await api('/export/bundles', { kind: 'history', scope, audience, document_ids: documentIds });
    // Ask the server what this draft's exact contents digest to. Nothing leaves on either call.
    const preview = await api(`/export/bundles/${encodeURIComponent(draft.bundle_id)}/review`, { approve: false });
    renderExportPreview({ ...draft, ...preview, state: 'draft' });
    notice('Draft assembled. Nothing has left: review the item list and its digest below.');
  } catch (error) {
    notice(error.message, true);
  } finally {
    $('export-draft').disabled = false;
  }
});

// P19-T06. Keep evidence in memory only and discard late responses after lock,
// scope changes, filter changes or session selection. Explicit resume is bounded.
let externalState = { generation: 0, cursor: 0, sessionsCursor: 0, selected: null, rows: new Map(), filters: {} };
function clearExternalHistory() {
  externalState = { generation: externalState.generation + 1, cursor: 0, sessionsCursor: 0, selected: null, rows: new Map(), filters: {} };
  for (const id of ['external-sessions', 'external-events', 'external-artifact', 'external-status']) $(id).replaceChildren();
  $('external-more').hidden = true; $('external-resume').hidden = true;
}
function externalScope() {
  const s = externalState.selected;
  return { project_id: s.project_id, producer_id: s.producer_id, logical_session_id: s.logical_session_id };
}
async function discoverExternal(reset) {
  if (!token) return;
  if (reset) {
    clearExternalHistory();
    externalState.filters = { project_id: $('external-project').value.trim(), producer_id: $('external-producer').value.trim() };
  }
  const generation = externalState.generation, myEpoch = epoch;
  $('external-more').disabled = true;
  try {
    const data = await api(externalHistoryPath('sessions', { ...externalState.filters, after: externalState.sessionsCursor, limit: 25 }));
    if (!token || epoch !== myEpoch || generation !== externalState.generation) return;
    for (const s of data.sessions) {
      const button = node('button', `${s.project_id} / ${s.producer_id} / ${s.logical_session_id} (${s.event_count} committed events)`);
      button.type = 'button'; button.className = 'external-session';
      button.addEventListener('click', () => {
        externalState = { ...externalState, generation: externalState.generation + 1, selected: s, cursor: 0, rows: new Map(), loading: false };
        $('external-events').replaceChildren(); $('external-artifact').replaceChildren();
        $('external-resume').hidden = false; loadExternalActivity();
      });
      $('external-sessions').append(button);
    }
    externalState.sessionsCursor = data.next_cursor; $('external-more').hidden = !data.has_more;
    $('external-status').textContent = data.sessions.length ? 'Select a session to inspect evidence.' : 'No additional committed sessions match. This does not prove local capture is empty.';
  } catch (error) { if (token && myEpoch === epoch && generation === externalState.generation) $('external-status').textContent = error.message; }
  finally { $('external-more').disabled = false; }
}
function renderExternalActivity() {
  const groups = new Map();
  for (const row of externalState.rows.values()) {
    const instance = row.envelope.producer_instance_id;
    if (!groups.has(instance)) groups.set(instance, []);
    groups.get(instance).push(row);
  }
  $('external-events').replaceChildren();
  for (const [instance, rows] of groups) {
    rows.sort((a,b) => a.envelope.producer_sequence - b.envelope.producer_sequence || a.cursor - b.cursor);
    const section = node('section'); section.append(node('h3', `Producer instance ${instance}`));
    section.append(node('p', 'Sequence order within this instance only; no global clock order is implied.'));
    const workKey = e => JSON.stringify(e.event_type.startsWith('task.') ? ['task', e.task_id] : ['tool', e.invocation_id]);
    const terminal = new Set(rows.filter(r => /^(task\.(completed|interrupted)|tool\.(completed|failed))$/.test(r.envelope.event_type)).map(r => workKey(r.envelope)));
    for (const row of rows) {
      const e = row.envelope, card = node('article', undefined, 'panel external-event');
      card.dataset.eventId = e.event_id;
      card.append(node('h4', `${e.producer_sequence}: ${e.event_type}`));
      card.append(node('p', `Evidence ${e.event_id}; receipt ${row.receipt_id}; occurred ${e.occurred_at}; ingested ${row.ingested_at}`));
      card.append(node('p', `Handler transport: ${e.outcome.transport}; execution: ${e.outcome.execution}; exit code: ${e.outcome.exit_code ?? 'not applicable'}`));
      if (/^(task\.(started|output)|tool\.(admitted|started))$/.test(e.event_type) && !terminal.has(workKey(e))) card.append(node('p', 'Pending background/work outcome: no terminal evidence in loaded history. This is not proof it is still running.'));
      card.append(node('p', `Capture: ${e.capture.payload}; ${e.capture.truncated ? 'truncated' : 'not marked truncated'}; sanitized evidence, not original content. Redaction completeness is not guaranteed.`));
      card.append(node('p', e.capture.conversation === 'client_supplied' ? 'Client-supplied excerpt only; not a complete transcript.' : `Conversation ${e.capture.conversation}; not an empty conversation.`));
      if (e.event_type === 'message.observed') card.append(node('p', `Source client: ${e.payload.source_client}; role: ${e.payload.role}; session: ${e.logical_session_id}; invocation: ${e.invocation_id || 'not supplied'}; task: ${e.task_id || 'not supplied'}`));
      const detail = node('details'); detail.append(node('summary', 'Open supporting evidence'), node('pre', JSON.stringify(e.payload, null, 2))); card.append(detail);
      if (e.event_type === 'artifact.recorded') {
        const button = node('button', 'Open artifact evidence'); button.type = 'button';
        button.addEventListener('click', async () => {
          const generation = externalState.generation, myEpoch = epoch;
          try {
            const artifact = await api(externalHistoryPath('artifact', { ...externalScope(), event_id: e.event_id }));
            if (!token || myEpoch !== epoch || generation !== externalState.generation) return;
            $('external-artifact').replaceChildren(node('p', artifact.reason), node('pre', JSON.stringify(artifact.envelope, null, 2)));
          } catch (error) { if (token && myEpoch === epoch && generation === externalState.generation) $('external-artifact').replaceChildren(node('p', error.message)); }
        }); card.append(button);
      }
      section.append(card);
    }
    $('external-events').append(section);
  }
}
async function loadExternalActivity() {
  if (!token || !externalState.selected || externalState.loading) return;
  externalState.loading = true;
  const state = externalState, generation = state.generation, myEpoch = epoch;
  $('external-resume').disabled = true;
  try {
    const data = await api(externalHistoryPath('activity', { ...externalScope(), after: state.cursor, limit: 50 }));
    if (!token || myEpoch !== epoch || generation !== externalState.generation) return;
    for (const row of data.events) state.rows.set(row.receipt_id, row);
    state.cursor = data.next_cursor;
    renderExternalActivity();
    $('external-status').textContent = `${state.rows.size} loaded committed events. ${data.has_more ? 'More pages remain.' : 'Caught up to this response; check again for arrivals.'} Producer acknowledgement and unseen local backlog: unknown.`;
  } catch (error) { if (token && myEpoch === epoch && generation === externalState.generation) $('external-status').textContent = `Read failed; cursor preserved. ${error.message}`; }
  finally { state.loading = false; $('external-resume').disabled = false; }
}
$('external-search').addEventListener('submit', event => { event.preventDefault(); discoverExternal(true); });
$('external-more').addEventListener('click', () => discoverExternal(false));
$('external-resume').addEventListener('click', loadExternalActivity);
$('lock').addEventListener('click', clearExternalHistory);
$('scope').addEventListener('change', clearExternalHistory);
