'use strict';
const $ = id => document.getElementById(id);
let token = '';
let scope = sessionStorage.getItem('harness_scope') || 'global';
let session = sessionStorage.getItem('harness_session') || crypto.randomUUID();
let busy = false;
let epoch = 0;
$('scope').value = scope;
function notice(text, error = false) { $('notice').textContent = text; $('notice').classList.toggle('error', error); }
async function api(path, body) {
  const response = await fetch(path, { method: body === undefined ? 'GET' : 'POST', headers: { 'Authorization': `Bearer ${token}`, ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) }, body: body === undefined ? undefined : JSON.stringify(body) });
  const payload = await response.json().catch(() => ({ error: `Unexpected response (${response.status})` }));
  if (!response.ok) throw new Error(payload.error || `Request failed (${response.status})`);
  return payload;
}
function node(tag, text, cls) { const element = document.createElement(tag); if (text !== undefined) element.textContent = text; if (cls) element.className = cls; return element; }
function persistSession() { sessionStorage.setItem('harness_scope', scope); sessionStorage.setItem('harness_session', session); }
function message(role, text, status) {
  $('log').querySelector('.empty')?.remove();
  const box = node('div', undefined, `message ${role === 'user' ? 'user' : 'assistant'}`);
  box.append(node('strong', role === 'user' ? 'You' : 'Harness'), node('span', text));
  if (status && status !== 'complete') box.append(node('span', `Capture status: ${status}`, 'muted'));
  $('log').append(box); $('log').scrollTop = $('log').scrollHeight;
}
async function refreshStatus() {
  const myEpoch = epoch;
  const data = await api('/memory/status');
  if (!token || myEpoch !== epoch) return;
  $('pending').textContent = String(data.pending_confirmations);
  $('stats').textContent = `${data.active_memories} active memories · ${data.queued_jobs} queued/running jobs · ${data.failed_jobs} failed jobs`;
}
async function loadHistory() {
  const myEpoch = epoch;
  const data = await api(`/sessions/${encodeURIComponent(session)}/messages`);
  if (!token || myEpoch !== epoch) return;
  if (data.scope && data.scope !== scope) { session = crypto.randomUUID(); persistSession(); return; }
  $('log').replaceChildren();
  if (!data.messages.length) $('log').append(node('p', 'Start with a question. Memory suggestions arrive separately in the inbox.', 'empty'));
  for (const m of data.messages) message(m.role, m.content, m.status);
}
$('authform').addEventListener('submit', async event => {
  event.preventDefault(); token = $('token').value.trim();
  try { await api('/memory/status'); $('auth').hidden = true; $('workspace').hidden = false; $('connection').textContent = 'Connected'; $('token').value = ''; notice('Connected. Memory activation always requires your approval.'); persistSession(); await loadHistory(); await refreshStatus(); }
  catch (error) { token = ''; notice(error.message, true); }
});
$('lock').addEventListener('click', () => { token = ''; epoch++; $('workspace').hidden = true; $('auth').hidden = false; $('connection').textContent = 'Locked'; $('log').replaceChildren(); $('candidates').replaceChildren(); $('jobs').replaceChildren(); $('recalled').replaceChildren(); $('modelnames').textContent = ''; $('mainmodel').value = ''; $('extractmodel').value = ''; $('prompt').value = ''; $('file').value = ''; $('consent').checked = false; notice('Access token cleared from this tab.'); });
$('newchat').addEventListener('click', () => {
  if (busy) return notice('Wait for the current request to finish.', true);
  const next = $('scope').value.trim();
  if (!/^[A-Za-z0-9_.:-]{1,80}$/.test(next)) return notice('Use a scope of 1–80 letters, digits, _, -, . or :.', true);
  scope = next; session = crypto.randomUUID(); epoch++; persistSession(); $('log').replaceChildren(node('p', 'New conversation, same approved memory system.', 'empty')); $('recalltrace').hidden = true; notice(`New conversation in ${scope}.`);
});
$('chatform').addEventListener('submit', async event => {
  event.preventDefault(); if (busy) return;
  const prompt = $('prompt').value;
  if (!prompt.trim() || new TextEncoder().encode(prompt).length > 16000) return notice('Message must contain 1–16000 UTF-8 bytes.', true);
  busy = true; $('send').disabled = true; $('newchat').disabled = true; const myEpoch = epoch;
  notice('Capturing your message and requesting an answer…');
  try {
    const result = await api('/chat', { prompt, session_id: session, request_id: crypto.randomUUID(), scope });
    if (!token || myEpoch !== epoch) return;
    $('prompt').value = ''; await loadHistory();
    $('recalltrace').hidden = !result.recalled.length; $('recalled').replaceChildren();
    for (const memory of result.recalled) {
      const block = node('div', undefined, 'candidate'); block.append(node('strong', `${memory.key} · ${memory.scope} · revision ${memory.revision}`), node('p', memory.value), node('p', memory.evidence?.quote || 'Legacy evidence unavailable', 'muted')); $('recalled').append(block);
    }
    notice(result.redacted ? 'Sensitive-looking input was redacted. Answer saved; memory review is queued.' : 'Answer saved. Memory suggestions are being processed separately.'); await refreshStatus();
  } catch (error) { if (token && myEpoch === epoch) { notice(error.message, true); await loadHistory().catch(() => {}); } }
  finally { busy = false; $('send').disabled = false; $('newchat').disabled = false; }
});
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
