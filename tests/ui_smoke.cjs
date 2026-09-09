// Browser UI tests against a mocked API. Does NOT test the Rust server.
const {chromium}=require('playwright');
const fs=require('fs');const path=require('path');const assert=require('assert');
const root=path.resolve(__dirname,'..');const out=process.env.QA_DIR || path.join(root,'docs','qa');fs.mkdirSync(out,{recursive:true});
(async()=>{
 const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH||'/usr/local/bin/chromium',args:['--no-sandbox']});
 try{
  const page=await browser.newPage({viewport:{width:1120,height:900},colorScheme:'light'});const errors=[];page.on('pageerror',e=>errors.push(String(e)));
  let candidate=true,failConfirm=false,chatHistory=[],receipt=null,importCalls=0,retryCalls=0,reverts=0;
  const malicious='<img src=x onerror="window.INJECTED=1">';
  // P2-T03 fixture: one applied change whose diff carries hostile text, so the card is held to
  // colouring by class and to never parsing tool text as HTML.
  let change={id:'synthetic-change',step_id:'synthetic-step',path:'notes.md',action:'modify',applied:true,reverted_at:null,revertable:true,revert_note:null,created_at:'2026-09-09T05:41:00Z',diff:'--- a/notes.md\n+++ b/notes.md\n@@ -1,3 +1,3 @@\n keep\n-beta '+malicious+'\n+BETA\n'};
  const data={id:'synthetic-proposal',scope:'global',key:'preferred_language',value:'Rust for local tools. '+malicious,old_value:'Python for prototypes.',category:'preference',expected_revision:1,evidence:{quote:'I prefer Rust for local tools.'}};
  await page.route('http://127.0.0.1:8080/**',async route=>{
   const req=route.request();const u=new URL(req.url());const p=u.pathname;
   const staticFiles={'/':'index.html','/style.css':'style.css','/app.js':'app.js'};
   if(staticFiles[p]){return route.fulfill({contentType:p.endsWith('.css')?'text/css':p.endsWith('.js')?'text/javascript':'text/html',body:fs.readFileSync(path.join(root,'static',staticFiles[p]),'utf8')});}
   if(req.headers().authorization!=='Bearer test-token'){return route.fulfill({status:401,json:{error:'Bearer token required'}});}
   let result={};
   if(p==='/memory/status')result={active_memories:candidate?1:2,pending_confirmations:candidate?1:0,queued_jobs:0,failed_jobs:1};
   else if(p==='/scopes')result={scopes:[{scope:'global',root_path:null,permission_mode:'ask'}]};
   else if(p==='/sessions')result={sessions:[]};
   else if(p.startsWith('/sessions/'))result={scope:'global',messages:chatHistory,has_more:false};
   else if(p.startsWith('/chat/requests/'))result=receipt;
   else if(p==='/memory/candidates')result={candidates:candidate?[data]:[]};
   else if(p==='/memory/confirm'){
    if(failConfirm)return route.fulfill({status:409,json:{error:'Proposal conflicts with a newer revision; reload the inbox'}});
    candidate=false;result={status:JSON.parse(req.postData()).confirm?'approved':'rejected'};
   }else if(p==='/chat/submit'){
    const body=JSON.parse(req.postData());chatHistory=[{role:'user',content:body.prompt,status:'complete',generation_state:'complete',request_id:body.request_id},{role:'assistant',content:'Use a small Rust service with SQLite. Keep capture separate from extraction.',status:'complete'}];
    receipt={request_id:body.request_id,session_id:body.session_id,state:'complete',response:chatHistory[1].content,recalled:[{...data,revision:2}],redacted:false,memory_status:'pending',events:[]};result=receipt;
   }else if(p==='/jobs')result={jobs:[{id:'failed-job',status:retryCalls?'pending':'failed',scope:'global',attempts:3,error:'Extraction failed; check provider configuration.'}]};
   else if(p==='/jobs/failed-job/retry'){retryCalls++;result={status:'queued'};}
   else if(p==='/memory/ingest'){importCalls++;result={duplicate:false,chunks_queued:2,warnings:[]};}
   else if(p==='/config')result=req.method()==='GET'?{main:'synthetic-main',extraction:'synthetic-small'}:{status:'saved'};
   else if(p==='/models')result={data:[{id:malicious},{id:'synthetic-main'}]};
   else if(p==='/permissions')result={permissions:[]};
   else if(p==='/changes')result={changes:[change]};
   else if(p===`/changes/${change.id}/revert`){reverts++;change={...change,revertable:false,revert_note:'Already reverted',reverted_at:'2026-09-09T05:42:00Z'};result={status:'restored',path:change.path,recorded:true};}
   else return route.fulfill({status:404,json:{error:'Unexpected mock route'}});
   await route.fulfill({json:result});
  });
  const shot=async name=>{await page.screenshot({path:path.join(out,name+'.png'),fullPage:true});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth),'horizontal overflow: '+name);};
  await page.goto('http://127.0.0.1:8080');await page.fill('#token','test-token');await page.click('#authform button');await page.waitForFunction(()=>!document.getElementById('workspace').hidden);
  assert.strictEqual(await page.locator('#token').inputValue(),'');assert(!await page.evaluate(()=>JSON.stringify({...localStorage,...sessionStorage}).includes('test-token')));
  await page.fill('#prompt','Help me choose the next implementation step.');await page.click('#send');await page.waitForFunction(()=>document.getElementById('notice').textContent.startsWith('Answer saved'));
  // P2-T03: the rail's diff card, its per-line colouring and the undo behind it.
  await page.waitForSelector('#agent-changes .diff-card');
  const diffText=await page.locator('#changes-list .diff-body').first().innerText();
  assert(diffText.includes('-beta '+malicious)&&diffText.includes('+BETA'),'the diff renders as text: '+diffText);
  assert.strictEqual(await page.locator('#changes-list img').count(),0);assert.strictEqual(await page.evaluate(()=>window.INJECTED),undefined);
  assert.deepStrictEqual([await page.locator('.diff-line.add').count(),await page.locator('.diff-line.del').count(),await page.locator('.diff-line.meta').count()],[1,1,3]);
  await shot('conversation-desktop');
  await page.locator('#changes-list .diff-foot button').click();
  await page.waitForFunction(()=>document.getElementById('notice').textContent.startsWith('Reverted'));
  assert.strictEqual(reverts,1);
  await page.waitForFunction(()=>!document.querySelector('#changes-list .diff-foot button'));
  assert((await page.locator('#changes-list .diff-foot').first().innerText()).includes('Already reverted'),'a reverted card says so instead of offering another undo');
  await page.click('[data-view="memory"]');await page.waitForSelector('.candidate h3');
  assert.strictEqual(await page.locator('#candidates img').count(),0);assert.strictEqual(await page.evaluate(()=>window.INJECTED),undefined);
  await shot('memory-desktop');
  failConfirm=true;await page.locator('#candidates button').first().click();await page.waitForFunction(()=>document.getElementById('notice').textContent.includes('conflicts'));
  assert(await page.locator('#candidates button').first().isEnabled());
  await page.setViewportSize({width:390,height:844});await shot('memory-conflict-mobile');
  await page.emulateMedia({colorScheme:'dark'});await shot('memory-conflict-dark-mobile');await page.emulateMedia({colorScheme:'light'});await page.setViewportSize({width:1120,height:900});
  failConfirm=false;await page.locator('#candidates button').first().click();await page.waitForSelector('#candidates .empty');
  await page.click('[data-view="imports"]');await page.waitForSelector('.job');await shot('imports-desktop');
  await page.locator('#jobs button').click();assert.strictEqual(retryCalls,1);
  await page.setInputFiles('#file',{name:'sample.jsonl',mimeType:'application/json',buffer:Buffer.from('{"type":"message","message":{"role":"user","content":"I prefer Rust"}}')});
  await page.check('#consent');await page.click('#importbutton');await page.waitForFunction(()=>document.getElementById('notice').textContent.includes('Queued 2'));assert.strictEqual(importCalls,1);
  await page.click('[data-view="settings"]');await page.waitForFunction(()=>document.getElementById('mainmodel').value==='synthetic-main');await page.click('#loadmodels');await page.waitForFunction(()=>document.getElementById('modelnames').textContent.includes('onerror'));
  assert.strictEqual(await page.locator('#modelnames img').count(),0);await shot('settings-desktop');
  await page.click('#lock');assert(await page.locator('#workspace').isHidden());assert.strictEqual(await page.locator('#candidates').innerText(),'');
  assert.deepStrictEqual(errors,[]);
  const result={status:'passed',scope:'Mocked API browser checks; Rust server not executed',checks:['connect','token_not_persisted','conversation','diff_card_rendered','diff_card_revert','memory_evidence','html_injection_rendered_as_text','failed_approval_recoverable','approval','import','job_retry','model_settings','mobile_overflow','dark_mode','lock','no_javascript_exceptions']};
  fs.writeFileSync(path.join(out,'ui-results.json'),JSON.stringify(result,null,2));console.log(JSON.stringify(result));
 }finally{await browser.close();}
})().then(()=>process.exit(0)).catch(e=>{console.error(e);process.exit(1)});
