// Browser UI tests against a mocked API. Does NOT test the Rust server.
const {chromium}=require('playwright');
const fs=require('fs');const path=require('path');const assert=require('assert');
const root=path.resolve(__dirname,'..');const out=process.env.QA_DIR || path.join(root,'docs','qa');fs.mkdirSync(out,{recursive:true});
(async()=>{
 const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH||'/usr/local/bin/chromium',args:['--no-sandbox']});
 try{
  const page=await browser.newPage({viewport:{width:1120,height:900},colorScheme:'light'});const errors=[];page.on('pageerror',e=>errors.push(String(e)));
  let importCandidate=true,inlineCandidate=true,inlineData=null,failConfirm=false,chatHistory=[],receipt=null,importCalls=0,retryCalls=0,reverts=0,editCalls=0,savedConfig=null;
  const malicious='<img src=x onerror="window.INJECTED=1">';
  // P2-T03 fixtures: a hostile modify diff proves inert rendering; a create proves its distinct
  // undo result and user-facing copy rather than assuming it behaves like a modification.
  let changes=[
   {id:'synthetic-change',step_id:'synthetic-step',path:'notes.md',action:'modify',applied:true,reverted_at:null,revertable:true,revert_note:null,created_at:'2026-09-09T05:41:00Z',diff:'--- a/notes.md\n+++ b/notes.md\n@@ -1,3 +1,3 @@\n keep\n-beta '+malicious+'\n+BETA\n'},
   {id:'synthetic-create',step_id:'synthetic-step',path:'draft.md',action:'create',applied:true,reverted_at:null,revertable:true,revert_note:null,created_at:'2026-09-09T05:41:01Z',diff:'--- a/draft.md\n+++ b/draft.md\n@@ -1,0 +1,1 @@\n+first draft\n'}
  ];
  // P5-T01 fixtures: the verdict is advisory, and its claim text is written by a model that read
  // tool output, so hostile text must stay inert in the badge and in its tooltip.
  const agentSteps=[
   {id:'synthetic-step',seq:0,kind:'model_call',status:'complete',tool_name:null,tool_call_id:null,summary:null,input_preview:null,output_preview:null,previews_capped:false,output_bytes:0,truncated:false,tokens_in:11,tokens_out:7,error_code:null,started_at:'2026-09-09T05:41:00Z',finished_at:'2026-09-09T05:41:01Z'},
   {id:'synthetic-verify',seq:1,kind:'verification',status:'complete',tool_name:null,tool_call_id:null,summary:null,input_preview:null,output_preview:null,previews_capped:false,output_bytes:0,truncated:false,tokens_in:null,tokens_out:null,error_code:null,started_at:'2026-09-09T05:41:02Z',finished_at:'2026-09-09T05:41:03Z'}
  ];
  const verification={status:'unverified',unverified_claims:1,step_id:'synthetic-verify',step_status:'complete',model:'synthetic-verifier',error_code:null,projection_capped:false,
   claims:[{claim:'The suite passes. '+malicious,status:'unverified',evidence_step_ids:[],reason:'No recorded step ran the suite. '+malicious}],
   skipped_diagnostics:['no test command ran in this turn '+malicious]};
  const data={id:'synthetic-proposal',scope:'global',key:'preferred_language',value:'Rust for local tools. '+malicious,old_value:'Python for prototypes.',category:'preference',expected_revision:1,request_id:null,priority:'normal',evidence:{quote:'I prefer Rust for local tools.'}};
  await page.route('http://127.0.0.1:8080/**',async route=>{
   const req=route.request();const u=new URL(req.url());const p=u.pathname;
   const staticFiles={'/':'index.html','/style.css':'style.css','/app.js':'app.js'};
   if(staticFiles[p]){return route.fulfill({contentType:p.endsWith('.css')?'text/css':p.endsWith('.js')?'text/javascript':'text/html',body:fs.readFileSync(path.join(root,'static',staticFiles[p]),'utf8')});}
   if(req.headers().authorization!=='Bearer test-token'){return route.fulfill({status:401,json:{error:'Bearer token required'}});}
   let result={};
   if(p==='/memory/status')result={active_memories:importCandidate?1:2,pending_confirmations:Number(importCandidate)+Number(inlineCandidate&&inlineData),queued_jobs:0,failed_jobs:1};
   else if(p==='/scopes')result={scopes:[{scope:'global',root_path:null,permission_mode:'ask'}]};
   else if(p==='/sessions')result={sessions:[]};
   else if(p.startsWith('/sessions/'))result={scope:'global',messages:chatHistory,has_more:false};
   else if(p.startsWith('/chat/requests/')&&p.endsWith('/steps'))result={steps:agentSteps,verification};
   else if(p.startsWith('/chat/requests/'))result=receipt;
   else if(p==='/memory/candidates'){
    const imports=u.searchParams.get('imports_only')==='true',chat=u.searchParams.get('chat_only')==='true';
    result={candidates:imports?(importCandidate?[data]:[]):chat?(inlineCandidate&&inlineData?[inlineData]:[]):[...(importCandidate?[data]:[]),...(inlineCandidate&&inlineData?[inlineData]:[])]};
   }else if(p.startsWith('/memory/candidates/')&&p.endsWith('/edit')){
    editCalls++;const body=JSON.parse(req.postData());inlineData={...inlineData,value:body.value,evidence:{...inlineData.evidence,edited:true}};result={status:'edited'};
   }else if(p==='/memory/confirm'){
    const body=JSON.parse(req.postData());if(body.confirmation_id===data.id&&failConfirm)return route.fulfill({status:409,json:{error:'Proposal conflicts with a newer revision; reload the inbox'}});
    if(body.confirmation_id===data.id)importCandidate=false;else inlineCandidate=false;result={status:body.confirm?'approved':'rejected'};
   }else if(p==='/chat/submit'){
    const body=JSON.parse(req.postData());inlineData={...data,id:'inline-proposal',key:'database_choice',value:'SQLite for the project. '+malicious,old_value:'Postgres',category:'decision',request_id:body.request_id,priority:'high',evidence:{quote:'No, use SQLite instead.'}};chatHistory=[{role:'user',content:body.prompt,status:'complete',generation_state:'complete',request_id:body.request_id},{role:'assistant',content:'Use a small Rust service with SQLite. Keep capture separate from extraction.',status:'complete'}];
    receipt={request_id:body.request_id,session_id:body.session_id,state:'complete',response:chatHistory[1].content,recalled:[{...data,revision:2}],redacted:false,memory_status:'pending',events:[]};result=receipt;
   }else if(p==='/jobs')result={jobs:[{id:'failed-job',status:retryCalls?'pending':'failed',scope:'global',attempts:3,error:'Extraction failed; check provider configuration.'}]};
   else if(p==='/jobs/failed-job/retry'){retryCalls++;result={status:'queued'};}
   else if(p==='/memory/ingest'){importCalls++;result={duplicate:false,chunks_queued:2,warnings:[]};}
   else if(p==='/config'){
    if(req.method()==='GET')result={main:'synthetic-main',extraction:'synthetic-small',verification:'synthetic-verifier'};
    else {savedConfig=JSON.parse(req.postData());result={status:'saved'};}
   }
   else if(p==='/models')result={data:[{id:malicious},{id:'synthetic-main'}]};
   else if(p==='/permissions')result={permissions:[]};
   else if(p==='/changes')result={changes};
   else if(p.startsWith('/changes/')&&p.endsWith('/revert')){
    const changed=changes.find(change=>p===`/changes/${change.id}/revert`);
    if(!changed)return route.fulfill({status:404,json:{error:'Unexpected change id'}});
    reverts++;changes=changes.map(change=>change.id===changed.id?{...change,revertable:false,revert_note:'Already reverted',reverted_at:'2026-09-09T05:42:00Z'}:change);
    result={status:changed.action==='create'?'deleted':'restored',path:changed.path,recorded:true};
   }
   else return route.fulfill({status:404,json:{error:'Unexpected mock route'}});
   await route.fulfill({json:result});
  });
  const shot=async name=>{await page.screenshot({path:path.join(out,name+'.png'),fullPage:true});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth),'horizontal overflow: '+name);};
  await page.goto('http://127.0.0.1:8080');await page.fill('#token','test-token');await page.click('#authform button');await page.waitForFunction(()=>!document.getElementById('workspace').hidden);
  assert.strictEqual(await page.locator('#token').inputValue(),'');assert(!await page.evaluate(()=>JSON.stringify({...localStorage,...sessionStorage}).includes('test-token')));
  await page.fill('#prompt','Help me choose the next implementation step.');await page.click('#send');await page.waitForFunction(()=>document.getElementById('notice').textContent.startsWith('Answer saved'));
  await page.waitForSelector('.suggestion-tray:not([hidden]) .candidate');
  const inline=page.locator('.suggestion-tray:not([hidden]) .candidate').first();assert.deepStrictEqual(await inline.locator('.row > button').allTextContents(),['Save','Edit','Dismiss']);
  assert.strictEqual(await inline.locator('img').count(),0);await inline.getByRole('button',{name:'Edit'}).click();await inline.locator('textarea').fill('SQLite with WAL');await inline.getByRole('button',{name:'Apply edit'}).click();
  await page.waitForFunction(()=>document.querySelector('.suggestion-tray:not([hidden])')?.textContent.includes('SQLite with WAL'));assert.strictEqual(editCalls,1);
  await page.locator('.suggestion-tray:not([hidden]) .candidate').getByRole('button',{name:'Dismiss'}).click();await page.waitForFunction(()=>document.querySelector('.suggestion-tray')?.hidden===true);
  // P2-T03: the rail's diff card, its per-line colouring and the undo behind it.
  await page.waitForFunction(()=>document.querySelectorAll('#changes-list .diff-card').length===2);
  const diffText=await page.locator('#changes-list .diff-body').first().innerText();
  assert(diffText.includes('-beta '+malicious)&&diffText.includes('+BETA'),'the diff renders as text: '+diffText);
  assert.strictEqual(await page.locator('#changes-list img').count(),0);assert.strictEqual(await page.evaluate(()=>window.INJECTED),undefined);
  const modifyCard=page.locator('#changes-list .diff-card').first();
  assert.deepStrictEqual([await modifyCard.locator('.diff-line.add').count(),await modifyCard.locator('.diff-line.del').count(),await modifyCard.locator('.diff-line.meta').count()],[1,1,3]);
  // P5-T01: the advisory verdict reaches the turn header, and its hostile claim text is quoted as
  // inert text in the tooltip rather than parsed as markup.
  await page.waitForFunction(()=>document.getElementById('verification-badge')?.hidden===false);
  const badge=page.locator('#verification-badge');
  assert.strictEqual((await badge.innerText()).trim(),'1 unverified');
  assert(await badge.evaluate(el=>el.classList.contains('unverified')),'the badge carries its status class');
  const tip=await badge.getAttribute('title');
  assert(tip.includes('No recorded step ran the suite. '+malicious)&&tip.includes('no test command ran in this turn'),'the tooltip quotes claim, reason and diagnostics: '+tip);
  assert.strictEqual(await page.locator('#verification-badge img').count(),0);assert.strictEqual(await page.evaluate(()=>window.INJECTED),undefined);
  assert((await page.locator('#steps-list').innerText()).includes('verification'),'the verification step is listed with the others');
  await shot('conversation-desktop');
  await modifyCard.locator('.diff-foot button').click();
  await page.waitForFunction(()=>document.getElementById('notice').textContent.startsWith('Reverted'));
  assert.strictEqual(reverts,1);
  await page.waitForFunction(()=>!document.querySelector('#changes-list .diff-card:first-child .diff-foot button'));
  assert((await page.locator('#changes-list .diff-card').first().locator('.diff-foot').innerText()).includes('Already reverted'),'a reverted card says so instead of offering another undo');
  const createCard=page.locator('#changes-list .diff-card').nth(1);
  await createCard.locator('.diff-foot button').click();
  await page.waitForFunction(()=>document.getElementById('notice').textContent.includes('file this turn created was removed'));
  assert.strictEqual(reverts,2);
  await page.waitForFunction(()=>!document.querySelector('#changes-list .diff-card:nth-child(2) .diff-foot button'));
  assert((await page.locator('#changes-list .diff-card').nth(1).locator('.diff-foot').innerText()).includes('Already reverted'),'an undone create also stops offering Revert');
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
  await page.click('[data-view="settings"]');await page.waitForFunction(()=>document.getElementById('mainmodel').value==='synthetic-main');
  assert.strictEqual(await page.locator('#verificationmodel').inputValue(),'synthetic-verifier');
  await page.fill('#verificationmodel','synthetic-verifier-2');await page.click('#settingsform button[type="submit"]');
  await page.waitForFunction(()=>document.getElementById('notice').textContent.includes('Model settings saved'));
  assert.strictEqual(savedConfig.verification,'synthetic-verifier-2');
  await page.click('#loadmodels');await page.waitForFunction(()=>document.getElementById('modelnames').textContent.includes('onerror'));
  assert.strictEqual(await page.locator('#modelnames img').count(),0);await shot('settings-desktop');
  await page.click('#lock');assert(await page.locator('#workspace').isHidden());assert.strictEqual(await page.locator('#candidates').innerText(),'');
  assert.strictEqual(await page.locator('#verificationmodel').inputValue(),'');
  assert.deepStrictEqual(errors,[]);
  const result={status:'passed',scope:'Mocked API browser checks; Rust server not executed',checks:['connect','token_not_persisted','conversation','inline_suggestion_tray','suggestion_edit','suggestion_dismiss','diff_card_rendered','diff_card_revert','diff_card_create_revert','verification_badge','verification_claim_text_inert','verification_model_setting','import_inbox_only','memory_evidence','html_injection_rendered_as_text','failed_approval_recoverable','suggestion_save','import','job_retry','model_settings','mobile_overflow','dark_mode','lock','no_javascript_exceptions']};
  fs.writeFileSync(path.join(out,'ui-results.json'),JSON.stringify(result,null,2));console.log(JSON.stringify(result));
 }finally{await browser.close();}
})().then(()=>process.exit(0)).catch(e=>{console.error(e);process.exit(1)});
