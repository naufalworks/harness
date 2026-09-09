'use strict';
const $ = id => document.getElementById(id);
let token = '';
let scope = sessionStorage.getItem('harness_scope') || 'global';
let session = sessionStorage.getItem('harness_session') || crypto.randomUUID();
let busy = false, epoch = 0, historyCursor = null, sessionsCursor = null;
let pending = null, pendingPrompt = null;
try { pending = JSON.parse(sessionStorage.getItem('harness_pending') || 'null'); } catch { sessionStorage.removeItem('harness_pending'); }
if (pending && (typeof pending.request_id !== 'string' || pending.session_id !== session || pending.scope !== scope)) pending = null;
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
    }
  }
  target.append(box);
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
  if (older) { const top = $('log').scrollHeight; $('log').prepend(fragment); $('log').scrollTop += $('log').scrollHeight - top; }
  else { $('log').replaceChildren(fragment); $('log').scrollTop = $('log').scrollHeight; }
  if (!data.messages.length && !older) $('log').append(node('p', 'Say hi to get started. Harness keeps up with the conversation and remembers what matters.', 'empty'));
  historyCursor = data.next_before_seq; $('oldermessages').hidden = !data.has_more;
  if (!older) {
    const last = [...data.messages].reverse().find(m => m.role === 'user');
    captureLabel(last ? stateLabels[last.generation_state] || '' : '');
    if (!pending && last?.request_id && ['captured','generating'].includes(last.generation_state)) rememberPending({request_id:last.request_id,session_id:session,scope});
  }
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
      session = item.id; scope = item.scope; epoch++; $('scope').value = scope; persistSession(); $('sessionhistory').open = false;
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
async function followReceipt(first, myEpoch) {
  let data = first;
  for (let i=0; i<120; i++) {
    if (!token || myEpoch !== epoch) return;
    accepted(data);
    await refreshAgentTurn(data).catch(() => {});
    if (['complete','failed','interrupted'].includes(data.state)) {
      rememberPending(null); $('retryrequest').hidden = true;
      await loadHistory(); await loadSessions();
      if (data.state === 'complete') notice(data.redacted ? 'Answer saved. Sensitive-looking input was filtered before saving.' : 'Answer saved on this device.');
      else notice(data.state === 'interrupted' ? 'Your message is saved. The server restarted before the answer completed; nothing was resent.' : 'Your message is saved, but the answer did not complete.', true);
      return;
    }
    if (i === 0) { await loadHistory(); notice('Your message is saved. You can reconnect later to check the answer.'); }
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
  if (!retry) { pendingPrompt = prompt; rememberPending({request_id:crypto.randomUUID(),session_id:session,scope}); }
  let admitted = false;
  const myEpoch = epoch; setBusy(true); captureLabel('Sending…');
  try {
    const result = await api('/chat/submit', {prompt, ...pending}); admitted = true;
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
  token = ''; epoch++; setBusy(false); pendingPrompt = null;
  $('workspace').hidden = true; $('auth').hidden = false; $('connection').textContent = 'Locked';
  for (const id of ['log','candidates','jobs','recalled','sessionlist']) $(id).replaceChildren();
  $('modelnames').textContent = ''; $('stats').textContent = ''; $('mainmodel').value = ''; $('extractmodel').value = ''; $('prompt').value = ''; $('file').value = ''; $('consent').checked = false;
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
  epoch++; setBusy(false); rememberPending(null); $('retryrequest').hidden = true; $('prompt').value = '';
  session = crypto.randomUUID(); persistSession(); historyCursor = null; $('oldermessages').hidden = true;
  $('log').replaceChildren(node('p', 'New conversation. Check History later for the previous answer.', 'empty')); captureLabel(''); notice('Nothing was resent. Any saved work remains in History.');
});
$('oldermessages').addEventListener('click', async () => { $('oldermessages').disabled = true; try { await loadHistory(true); } catch(e) { notice(e.message,true); } finally { $('oldermessages').disabled = false; } });
$('oldersessions').addEventListener('click', () => loadSessions(true).catch(e=>notice(e.message,true)));
$('sessionhistory').addEventListener('toggle', () => { if ($('sessionhistory').open) loadSessions().catch(e=>notice(e.message,true)); });
async function loadCandidates() {
  const myEpoch = epoch; const data = await api(`/memory/candidates?scope=${encodeURIComponent(scope)}`);
  if (!token || myEpoch !== epoch) return;
  $('candidates').replaceChildren();
  if (!data.candidates.length) $('candidates').append(node('p', 'No pending suggestions in this scope. Extraction runs in the background; refresh after jobs finish.', 'empty'));
  for (const c of data.candidates) {
    const card = node('article', undefined, 'candidate'); card.append(node('h3', c.key), node('p', `${c.category} · ${c.scope} · based on revision ${c.expected_revision}`, 'muted'), node('p', `Previously: ${c.old_value ?? 'Not remembered'}`), node('p', `Proposed: ${c.value}`), node('blockquote', c.evidence?.quote || 'Legacy import: original evidence is unavailable.'));
    const controls = node('div', undefined, 'row');
    for (const [label, confirm] of [['Approve memory', true], ['Reject', false]]) {
      const button = node('button', label, confirm ? '' : 'secondary'); button.type = 'button';
      button.addEventListener('click', async () => {
        for (const b of controls.querySelectorAll('button')) b.disabled = true;
        try { const out = await api('/memory/confirm', { confirmation_id: c.id, confirm, scope: c.scope }); notice(`Memory ${out.status}.`); await loadCandidates(); await refreshStatus(); }
        catch (error) { notice(error.message, true); for (const b of controls.querySelectorAll('button')) b.disabled = false; }
      }); controls.append(button);
    }
    card.append(controls); $('candidates').append(card);
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
$('settingsform').addEventListener('submit', async event => { event.preventDefault(); try { await api('/config', { main: $('mainmodel').value.trim(), extraction: $('extractmodel').value.trim() }); notice('Model settings saved.'); } catch (error) { notice(error.message, true); } });
$('loadmodels').addEventListener('click', async () => { try { const data = await api('/models'); $('modelnames').textContent = data.data.map(m => String(m.id)).join('\n'); } catch (error) { notice(error.message, true); } });
$('refreshmemory').addEventListener('click', () => loadCandidates().catch(e => notice(e.message, true)));
$('refreshjobs').addEventListener('click', () => loadJobs().catch(e => notice(e.message, true)));
for (const tab of document.querySelectorAll('[data-view]')) tab.addEventListener('click', async () => {
  for (const t of document.querySelectorAll('[data-view]')) { const active = t === tab; t.classList.toggle('active', active); t.setAttribute('aria-pressed', String(active)); $(`view-${t.dataset.view}`).hidden = !active; }
  try { if (tab.dataset.view === 'memory') await loadCandidates(); if (tab.dataset.view === 'imports') await loadJobs(); if (tab.dataset.view === 'settings') { const data = await api('/config'); $('mainmodel').value = data.main || ''; $('extractmodel').value = data.extraction || ''; } } catch (error) { notice(error.message, true); }
});
setInterval(() => { if (token && !document.hidden) refreshStatus().catch(() => {}); }, 5000);

// P1-T13: the agent activity view is deliberately polling for now. The durable receipt remains
// the source of truth, while these read-only endpoints make the current turn visible between
// model calls. No model/tool text is inserted as HTML.
const agentState = { requestId: null, sessionId: null, scope: null, permission: null, busyDecision: false };

function agentIcon(status) {
  if (status === 'complete') return '●';
  if (status === 'failed') return '✖';
  if (status === 'denied' || status === 'interrupted') return '⚠';
  return '○';
}

function agentMeta(step) {
  if (step.status === 'running') return 'running';
  if (step.status === 'queued') return 'queued';
  if (step.finished_at) {
    const date = new Date(step.finished_at);
    if (!Number.isNaN(date.getTime())) return date.toLocaleTimeString([], {hour:'2-digit', minute:'2-digit'});
  }
  return step.status || '';
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

async function refreshAgentTurn(receipt) {
  const requestId = receipt?.request_id || pending?.request_id;
  const sessionId = receipt?.session_id || pending?.session_id || session;
  const turnScope = receipt?.scope || pending?.scope || scope;
  if (!token || !requestId) return;
  const data = await Promise.all([
    api(`/chat/requests/${encodeURIComponent(requestId)}/steps`),
    api(`/sessions/${encodeURIComponent(sessionId)}/plan`),
    api(`/permissions?scope=${encodeURIComponent(turnScope)}`),
  ]);
  if (!token || requestId !== (pending?.request_id || requestId) || sessionId !== session) return;
  agentState.requestId = requestId; agentState.sessionId = sessionId; agentState.scope = turnScope;
  $('agent-turn').hidden = false;
  $('agent-turn-status').textContent = agentStatusLabel(receipt?.state);
  renderAgentSteps(data[0].steps || []);
  renderAgentPlan(data[1]);
  const match = (data[2].permissions || []).find(item => item.request_id === requestId);
  renderAgentPermission(match || null);
  if (!match && !(data[0].steps || []).length && !(data[1].items || []).length) $('agent-turn').hidden = true;
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

// Keep the activity card alive after the user returns to a tab with an unfinished request.
setInterval(() => {
  if (token && pending && !document.hidden) refreshAgentTurn().catch(() => {});
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
