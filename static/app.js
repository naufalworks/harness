'use strict';
const $ = id => document.getElementById(id);
let token = '';
let scope = sessionStorage.getItem('harness_scope') || 'global';
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
function captureLabel(text) { $('capturestatus').textContent = text; }
function rememberPending(value) {
  pending = value;
  if (value) sessionStorage.setItem('harness_pending', JSON.stringify(value));
  else { sessionStorage.removeItem('harness_pending'); pendingPrompt = null; }
  $('checkrecording').hidden = !value; $('leavepending').hidden = !value; $('send').hidden = !!value;
}
async function api(path, body) {
  const controller = new AbortController(); const timer = setTimeout(() => controller.abort(), 20000);
  try {
    const response = await fetch(path, { method: body === undefined ? 'GET' : 'POST', signal: controller.signal, headers: { 'Authorization': `Bearer ${token}`, ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) }, body: body === undefined ? undefined : JSON.stringify(body) });
    const payload = await response.json().catch(() => ({ error: `Unexpected response (${response.status})` }));
    if (!response.ok) { const error = new Error(payload.error || `Request failed (${response.status})`); error.status = response.status; throw error; }
    return payload;
  } finally { clearTimeout(timer); }
}
function node(tag, text, cls) { const element = document.createElement(tag); if (text !== undefined) element.textContent = text; if (cls) element.className = cls; return element; }
function persistSession() { sessionStorage.setItem('harness_scope', scope); sessionStorage.setItem('harness_session', session); }
const stateLabels = { captured: 'Sent · waiting for answer', generating: 'Thinking…', complete: 'Done', failed: 'Saved · answer failed', interrupted: 'Saved · answer interrupted' };
const eventLabels = { captured: 'Message saved locally', generation_started: 'Answer started', context_saved: 'Context receipt saved', answer_saved: 'Answer saved locally', generation_failed: 'Answer did not complete', interrupted: 'Server restarted; no automatic resend', extraction_queued: 'Memory review queued' };
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
    target.append(terminal);
  }
}
async function refreshStatus() {
  const myEpoch = epoch; const data = await api('/memory/status');
  if (!token || myEpoch !== epoch) return;
  $('pending').textContent = String(data.pending_confirmations);
  $('stats').textContent = `${data.active_memories} memories remembered${data.queued_jobs ? ` · ${data.queued_jobs} jobs queued` : ''}${data.failed_jobs ? ` · ${data.failed_jobs} failed` : ''}`;
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
  if (!older) {
    const last = [...data.messages].reverse().find(m => m.role === 'user');
    captureLabel(last ? stateLabels[last.generation_state] || '' : '');
    if (!pending && last?.request_id && ['captured','generating'].includes(last.generation_state)) rememberPending({request_id:last.request_id,session_id:session,scope});
  }
  await refreshInlineSuggestions().catch(() => {});
}
async function loadSessions(older = false) {
  const myEpoch = epoch;
  const data = await api('/sessions' + (older && sessionsCursor ? `?before_seq=${sessionsCursor}` : ''));
  if (!token || myEpoch !== epoch) return;
  if (!older) $('sessionlist').replaceChildren();
  for (const item of data.sessions) {
    const button = node('button', undefined, 'session-entry secondary'); button.type = 'button';
    button.append(node('strong', item.title), node('span', `${item.scope} · ${item.message_count} messages`, 'muted'));
    button.addEventListener('click', async () => {
      if (busy || pending) return notice('Let the current message finish before switching conversations.', true);
      session = item.id; scope = item.scope; epoch++; $('scope').value = scope; persistSession();
      // P2-T02: in the wide three-pane layout the sidebar stays put; only the mobile drawer closes.
      if (window.matchMedia('(max-width: 920px)').matches) { $('sessionhistory').open = false; $('sidebar').classList.remove('open'); $('drawerbg').classList.remove('show'); }
      await loadHistory().catch(e => notice(e.message, true)); refreshScopeSetup().catch(() => {}); if (pending) await resumeRecording();
    }); $('sessionlist').append(button);
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
      notice(error.status === 404 ? 'No receipt found yet. Nothing was resent. If your draft is still in this tab, Retry same message safely reuses its original request.' : 'Connection lost. The latest status is unknown—not necessarily lost. Check again; nothing will be resent automatically.', true);
      $('retryrequest').hidden = pendingPrompt === null;
    }
  } finally { if (myEpoch === epoch) setBusy(false); }
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
      if (!admitted && !retry && [400,401,403,413,422,503].includes(error.status)) {
        rememberPending(null); captureLabel('Message not accepted'); notice(error.message + ' Your draft remains in this tab.', true);
      } else {
        captureLabel('Needs checking'); notice('Could not confirm the result. Use Check status before sending again. Your draft stays in this tab.', true);
        $('retryrequest').hidden = pendingPrompt === null;
      }
    }
  } finally { if (myEpoch === epoch) setBusy(false); }
}
$('authform').addEventListener('submit', async event => {
  event.preventDefault(); token = $('token').value.trim(); const myEpoch = epoch;
  try {
    await api('/memory/status'); if (myEpoch !== epoch) return;
    $('auth').hidden = true; $('workspace').hidden = false; $('connection').textContent = 'Connected'; $('token').value = '';
    notice('Connected — pick up where you left off.'); persistSession(); await loadHistory(); await loadSessions(); await refreshStatus(); await refreshScopeSetup();
    rememberPending(pending); if (pending) await resumeRecording();
  } catch (error) { token = ''; $('workspace').hidden = true; $('auth').hidden = false; notice(error.message, true); }
});
$('lock').addEventListener('click', () => {
  token = ''; epoch++; setBusy(false); pendingPrompt = null; closeGenerationStream(); closeActivityStream();
  $('workspace').hidden = true; $('auth').hidden = false; $('connection').textContent = 'Locked';
  for (const id of ['log','candidates','jobs','recalled','sessionlist']) $(id).replaceChildren();
  $('modelnames').textContent = ''; $('stats').textContent = ''; $('mainmodel').value = ''; $('extractmodel').value = ''; $('verificationmodel').value = ''; $('prompt').value = ''; $('file').value = ''; $('consent').checked = false;
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
$('leavepending').addEventListener('click', () => {
  if (!pending || !window.confirm('Start a new conversation without resending or cancelling this request? Any saved work remains in History. Unsaved draft text will be cleared.')) return;
  epoch++; setBusy(false); closeGenerationStream(); closeActivityStream(); rememberPending(null); $('retryrequest').hidden = true; $('prompt').value = '';
  session = crypto.randomUUID(); persistSession(); historyCursor = null; $('oldermessages').hidden = true;
  $('log').replaceChildren(node('p', 'New conversation. Check History later for the previous answer.', 'empty')); captureLabel(''); notice('Nothing was resent. Any saved work remains in History.');
});
$('oldermessages').addEventListener('click', async () => { $('oldermessages').disabled = true; try { await loadHistory(true); } catch(e) { notice(e.message,true); } finally { $('oldermessages').disabled = false; } });
$('oldersessions').addEventListener('click', () => loadSessions(true).catch(e=>notice(e.message,true)));
$('sessionhistory').addEventListener('toggle', () => { if ($('sessionhistory').open) loadSessions().catch(e=>notice(e.message,true)); });
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
  controls.append(save, edit, dismiss); card.append(controls); return card;
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
$('settingsform').addEventListener('submit', async event => { event.preventDefault(); try { await api('/config', { main: $('mainmodel').value.trim(), extraction: $('extractmodel').value.trim(), verification: $('verificationmodel').value.trim() }); notice('Model settings saved.'); } catch (error) { notice(error.message, true); } });
$('loadmodels').addEventListener('click', async () => { try { const data = await api('/models'); $('modelnames').textContent = data.data.map(m => String(m.id)).join('\n'); } catch (error) { notice(error.message, true); } });
$('refreshmemory').addEventListener('click', () => loadCandidates().catch(e => notice(e.message, true)));
$('refreshjobs').addEventListener('click', () => loadJobs().catch(e => notice(e.message, true)));
for (const tab of document.querySelectorAll('[data-view]')) tab.addEventListener('click', async () => {
  for (const t of document.querySelectorAll('[data-view]')) { const active = t === tab; t.classList.toggle('active', active); t.setAttribute('aria-pressed', String(active)); $(`view-${t.dataset.view}`).hidden = !active; }
  try { if (tab.dataset.view === 'memory') await loadCandidates(); if (tab.dataset.view === 'imports') await loadJobs(); if (tab.dataset.view === 'settings') { const data = await api('/config'); $('mainmodel').value = data.main || ''; $('extractmodel').value = data.extraction || ''; $('verificationmodel').value = data.verification || ''; } } catch (error) { notice(error.message, true); }
});
setInterval(() => { if (token && !document.hidden) { refreshStatus().catch(() => {}); refreshInlineSuggestions().catch(() => {}); } }, 5000);

// P1-T13: the agent activity view reads the recorded rows; the durable receipt remains the
// source of truth, while these read-only endpoints make the current turn visible between model
// calls. No model/tool text is inserted as HTML.
const agentState = { requestId: null, sessionId: null, scope: null, permission: null, busyDecision: false, steps: [], changes: [], verification: null, incident: null, incidentNode: null };

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
  $('permission-summary').textContent = `${permission.tool}: ${permission.summary}`;
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
function incidentNode(graph, id) { return (graph?.nodes || []).find(item => item.id === id); }
function focusDurableRow(item) {
  const selector = item.kind === 'step' ? `.agent-step[data-step-id="${CSS.escape(item.row_id)}"]` : item.kind === 'mutation' ? `.diff-card[data-change-id="${CSS.escape(item.row_id)}"]` : '';
  const target = selector && document.querySelector(selector);
  if (!target) return notice('This durable row is identified in the graph but has no separate rail view.');
  if (target.tagName === 'DETAILS') target.open = true;
  target.scrollIntoView({block:'center',behavior:'smooth'}); target.focus?.({preventScroll:true});
}
function renderIncidentDetail(graph, item) {
  const detail = $('incident-detail'); detail.replaceChildren(); detail.hidden = !item;
  if (!item) return;
  detail.append(node('span', item.kind, `incident-kind ${item.kind}`), node('strong', item.label || item.id), node('p', `${item.status || 'recorded'} · provenance ${item.provenance || 'unknown'}`, 'muted'));
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
function renderIncident(graph) {
  const panel = $('agent-incident'), select = $('incident-relation'), list = $('incident-nodes'); list.replaceChildren();
  if (!graph?.nodes?.length) { panel.hidden = true; agentState.incident = null; return; }
  panel.hidden = false; agentState.incident = graph;
  if (select.options.length === 1) for (const relation of incidentRelations) select.append(new Option(relation.replace('_',' '), relation));
  const relation = select.value; const edges = graph.edges || [];
  const visibleIds = relation === 'all' ? new Set(graph.nodes.map(item => item.id)) : new Set(edges.filter(edge => edge.relation === relation).flatMap(edge => [edge.source, edge.target]));
  const visible = graph.nodes.filter(item => visibleIds.has(item.id));
  $('incident-count').textContent = `${visible.length} nodes · ${relation === 'all' ? edges.length : edges.filter(edge => edge.relation === relation).length} edges`;
  const earliest = graph.earliest_known_break || {};
  $('incident-break').textContent = earliest.known ? `Earliest known break: ${earliest.reason}` : 'Earliest break unknown';
  if (!visible.length) list.append(node('p', 'No durable edges use this relation.', 'muted'));
  for (const item of visible) {
    const button = node('button', undefined, `incident-node${agentState.incidentNode === item.id ? ' active' : ''}`); button.type = 'button';
    button.append(node('span', item.kind, `incident-kind ${item.kind}`), node('span', item.label || item.id, 'incident-label'), node('span', item.status || 'recorded', 'incident-status'));
    button.addEventListener('click', () => { agentState.incidentNode = item.id; renderIncident(graph); }); list.append(button);
  }
  const selected = incidentNode(graph, agentState.incidentNode) || incidentNode(graph, earliest.node_id) || visible[0];
  agentState.incidentNode = selected?.id || null; renderIncidentDetail(graph, selected);
}
$('incident-relation').addEventListener('change', () => renderIncident(agentState.incident));

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
  ]);
  if (!token || requestId !== (pending?.request_id || requestId) || sessionId !== session) return;
  agentState.requestId = requestId; agentState.sessionId = sessionId; agentState.scope = turnScope;
  $('agent-turn').hidden = false;
  // P2-T02: the turn record lives in the right rail; auto-open it on wide screens only.
  if (!window.matchMedia('(max-width: 1100px)').matches) $('rail').hidden = false;
  $('agent-turn-status').textContent = agentStatusLabel(receipt?.state);
  agentState.verification = data[0].verification || null;
  renderVerification(agentState.verification);
  agentState.steps = data[0].steps || [];
  renderAgentSteps(agentState.steps);
  renderAgentPlan(data[1]);
  agentState.changes = data[3].changes || [];
  renderAgentChanges(agentState.changes);
  renderIncident(data[4]);
  // P2 context meter placeholder: real tokens-so-far from step receipts; the budget bar lands in P3.
  const tokens = (data[0].steps || []).reduce((sum, s) => sum + (s.tokens_in || 0) + (s.tokens_out || 0), 0);
  $('context-tokens').textContent = tokens ? `${tokens.toLocaleString()} tokens so far` : 'meter lands in P3';
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

// The turn's own clock. While the stream is live this only re-renders the running step's
// elapsed time from rows already fetched; with no stream it is the P1-T13 poll, unchanged.
setInterval(() => {
  if (!token || !pending || document.hidden) return;
  if (activityStream.live) { if (agentState.steps.length) renderAgentSteps(agentState.steps); }
  else refreshAgentTurn().catch(() => {});
}, 1000);

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
$('navtoggle').addEventListener('click', () => { $('sidebar').classList.toggle('open'); $('drawerbg').classList.toggle('show'); });
$('drawerbg').addEventListener('click', () => { $('sidebar').classList.remove('open'); $('drawerbg').classList.remove('show'); });
$('railbtn').addEventListener('click', () => { $('rail').hidden = !$('rail').hidden; });
$('railclose').addEventListener('click', () => { $('rail').hidden = true; });
// CLI-style composer: Enter sends, Shift+Enter keeps the newline (design: ui.md#keyboard).
$('prompt').addEventListener('keydown', event => {
  if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) { event.preventDefault(); $('chatform').requestSubmit(); }
});
