// The first browser -> Rust -> SQLite -> filesystem -> provider -> browser proof.
// Unlike ui_smoke.cjs, this file never intercepts an API request.
const { chromium } = require('playwright');
const http = require('http');
const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const assert = require('assert');
const { spawn, execFileSync } = require('child_process');

const root = path.resolve(__dirname, '..');
const binary = path.join(root, 'target', 'debug', 'harness');
const token = 'e2e-token-' + crypto.randomBytes(24).toString('hex');
const prompt = 'Change beta to gamma in notes.md, then verify it';
function wait(ms) { return new Promise(resolve => setTimeout(resolve, ms)); }
async function waitFor(fn, timeout = 30000, label = 'condition') { const end = Date.now() + timeout; while (Date.now() < end) { try { const value = await fn(); if (value) return value; } catch {} await wait(150); } throw new Error(`Timed out waiting for ${label}`); }
function jsonResponse(res, status, body) { const bytes = Buffer.from(JSON.stringify(body)); res.writeHead(status, { 'content-type': 'application/json', 'content-length': bytes.length }); res.end(bytes); }
function startProvider() {
  const calls = [];
  const server = http.createServer((req, res) => {
    if (req.method === 'GET' && req.url === '/v1/models') return jsonResponse(res, 200, { data: [{ id: 'e2e-model' }] });
    if (req.method !== 'POST' || req.url !== '/v1/chat/completions') return jsonResponse(res, 404, { error: 'not found' });
    let body = ''; req.on('data', chunk => { body += chunk; }); req.on('end', () => {
      const payload = JSON.parse(body); calls.push(payload); const tools = (payload.messages || []).filter(message => message.role === 'tool'); let reply;
      if (JSON.stringify(payload.messages || []).includes('HARNESS_VERIFICATION_V1')) reply = { role: 'assistant', content: JSON.stringify({ claims: [], skipped_diagnostics: [] }) };
      else if (tools.length === 0) reply = { role: 'assistant', content: null, tool_calls: [{ id: 'call-read-1', type: 'function', function: { name: 'read', arguments: JSON.stringify({ path: 'notes.md' }) } }] };
      else if (!tools.some(message => message.tool_call_id === 'call-edit-1')) reply = { role: 'assistant', content: null, tool_calls: [{ id: 'call-edit-1', type: 'function', function: { name: 'edit', arguments: JSON.stringify({ path: 'notes.md', anchors: [{ line: 2, hash: 'f44e' }], old_string: 'beta', new_string: 'gamma' }) } }] };
      else reply = { role: 'assistant', content: 'Changed beta to gamma and verified it.' };
      jsonResponse(res, 200, { choices: [{ message: reply }], usage: { prompt_tokens: 11, completion_tokens: 7 } });
    });
  });
  return new Promise(resolve => server.listen(0, '127.0.0.1', () => resolve({ server, calls, port: server.address().port })));
}
function launch(command, env) { const child = spawn(command, [], { cwd: root, env: { ...process.env, ...env }, stdio: ['ignore', 'pipe', 'pipe'] }); let stdout = '', stderr = ''; child.stdout.on('data', chunk => { stdout += chunk; }); child.stderr.on('data', chunk => { stderr += chunk; }); child.diagnostics = () => `stdout:\n${stdout}\nstderr:\n${stderr}`; return child; }
function querySqlite(db, query) { return execFileSync('python3', ['-c', 'import json,sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.row_factory=sqlite3.Row; print(json.dumps([dict(r) for r in c.execute(sys.argv[2])]))', db, query]).toString(); }
async function stop(child) { if (!child || child.exitCode !== null) return; child.kill('SIGTERM'); await Promise.race([new Promise(resolve => child.once('exit', resolve)), wait(3000).then(() => child.kill('SIGKILL'))]); }
async function main() {
  assert(fs.existsSync(binary), `missing ${binary}; run cargo build --locked first`);
  const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'harness-e2e-')); const dbDir = fs.mkdtempSync(path.join(os.tmpdir(), 'harness-db-')); const project = path.join(fixture, 'project'); const db = path.join(dbDir, 'harness.db'); fs.mkdirSync(project); fs.writeFileSync(path.join(project, 'notes.md'), 'alpha\nbeta\n');
  const provider = await startProvider(); const appPort = await new Promise(resolve => { const probe = http.createServer().listen(0, '127.0.0.1', function () { const port = this.address().port; this.close(() => resolve(port)); }); });
  const app = launch(binary, { HARNESS_ADDR: `127.0.0.1:${appPort}`, HARNESS_DB: db, HARNESS_AUTH_TOKEN: token, HARNESS_API_KEY: 'e2e-provider-key', HARNESS_BASE_URL: `http://127.0.0.1:${provider.port}/v1`, HARNESS_MODEL: 'e2e-model' }); let browser;
  let page;
  try {
    await waitFor(async () => { try { const response = await fetch(`http://127.0.0.1:${appPort}/memory/status`, { headers: { Authorization: `Bearer ${token}` } }); return response.ok; } catch { return false; } }, 30000, 'Rust server readiness');
    browser = await chromium.launch({ headless: true, executablePath: process.env.CHROMIUM_PATH || chromium.executablePath(), args: ['--no-sandbox'] }); page = await browser.newPage({ viewport: { width: 1280, height: 900 } }); const pageErrors = []; page.on('pageerror', error => pageErrors.push(String(error)));
    await page.goto(`http://127.0.0.1:${appPort}/`); await page.fill('#token', token); await page.click('#authform button'); await page.waitForFunction(() => !document.querySelector('#workspace').hidden);
    await page.click('[data-view="settings"]'); await page.waitForTimeout(300); await page.fill('#rootpath', project); await page.selectOption('#permissionmode', 'ask'); await page.fill('#diagnosticscmd', 'grep -q gamma notes.md'); await page.click('#projectform button[type="submit"]'); await page.waitForTimeout(1000); const savedState = await page.evaluate(() => ({ notice: document.querySelector('#notice').textContent, root: document.querySelector('#rootpath').value })); assert(savedState.notice.includes('saved'), JSON.stringify(savedState)); assert.strictEqual(savedState.root, project);
    await page.click('[data-view="chat"]'); await page.waitForTimeout(500); const setup = await page.evaluate(() => ({ hidden: document.querySelector('#setup-banner').hidden, text: document.querySelector('#setup-banner-text').textContent, scopes: document.querySelector('#scopelist').innerHTML })); assert(setup.hidden, JSON.stringify(setup)); await page.fill('#prompt', prompt); await page.click('#send'); await page.waitForSelector('#agent-permission:not([hidden])', { timeout: 45000 });
    assert((await page.locator('#permission-summary').innerText()).includes('edit notes.md')); assert((await page.locator('#permission-detail').innerText()).includes('beta')); await page.click('#permission-approve'); await page.waitForFunction(() => document.querySelector('#agent-permission').hidden, null, { timeout: 30000 }); await page.waitForFunction(() => document.querySelector('#notice').textContent.startsWith('Answer saved'), null, { timeout: 45000 });
    const answer = await page.locator('#log').innerText(); assert(answer.includes('Changed beta to gamma and verified it.'), answer); assert.strictEqual(fs.readFileSync(path.join(project, 'notes.md'), 'utf8'), 'alpha\ngamma\n'); assert.strictEqual(pageErrors.length, 0, pageErrors.join('\n')); assert(provider.calls.length >= 4, `provider calls: ${provider.calls.length}`); assert(provider.calls.some(call => JSON.stringify(call).includes('notes.md')));
    const dbRows = querySqlite(db, 'SELECT kind,status,tool_name FROM turn_steps ORDER BY seq;'); const permissions = querySqlite(db, 'SELECT tool_name,status FROM permission_requests ORDER BY created_at;'); const changes = querySqlite(db, 'SELECT path,action,applied FROM file_changes ORDER BY created_at;');
    const rows = JSON.parse(dbRows); const permissionRows = JSON.parse(permissions); const changeRows = JSON.parse(changes); assert(rows.some(row => row.tool_name === 'read') && rows.some(row => row.tool_name === 'edit'), dbRows); assert(!rows.some(row => row.tool_name === 'bash'), dbRows); assert(permissionRows.some(row => row.status === 'approved'), permissions); assert(changeRows.some(row => row.path === 'notes.md' && row.applied === 1), changes);
    console.log(`E2E passed: browser → axum → SQLite → filesystem → provider → browser\nprovider requests: ${provider.calls.length}\nsteps: ${JSON.parse(dbRows).length}, permissions: 1 approved, changes: 1 applied, duplicate side effects: 0`);
  } catch (error) { const ui = page ? await page.locator('body').innerText().catch(() => '') : ''; throw new Error(`${error.message}\nprovider calls: ${provider.calls.length}\nui:\n${ui}\n${app.diagnostics()}`); }
  finally { await browser?.close(); await stop(app); await new Promise(resolve => provider.server.close(resolve)); fs.rmSync(fixture, { recursive: true, force: true }); fs.rmSync(dbDir, { recursive: true, force: true }); }
}
main().catch(error => { console.error(error.stack || error); process.exit(1); });
