// Mocked API only. This checks frontend behavior, not the compiled Rust service.
const {chromium}=require('playwright');
const fs=require('fs'), path=require('path'), assert=require('assert');
const root=path.resolve(__dirname,'..'), out=path.join(root,'docs/qa');fs.mkdirSync(out,{recursive:true});
(async()=>{
 const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH||'/usr/local/bin/chromium',args:['--no-sandbox']});
 try {
  const page=await browser.newPage({viewport:{width:1120,height:900},colorScheme:'light'});
  const errors=[];page.on('pageerror',e=>errors.push(String(e)));
  const records=new Map();let submits=0, nextState='complete', abortSubmit=false, offline=false, hold=false, polls=0;
  const hostile='<img src=x onerror="window.INJECTED=1">';
  function receipt(item){return {request_id:item.request_id,session_id:item.session_id,scope:item.scope,model:'synthetic-main',state:hold?'generating':item.final,response:hold?null:item.final==='complete'?'Keep the conversation. Be selective about what becomes memory.':null,memory_status:'deferred',redacted:false,recalled:[],events:[{kind:'captured',at:'2026-09-08T13:01:00Z'},{kind:'generation_started',at:'2026-09-08T13:01:01Z'},{kind:'context_saved',at:'2026-09-08T13:01:01Z'},...(!hold&&item.final==='complete'?[{kind:'answer_saved',at:'2026-09-08T13:01:03Z'}]:[])],context:{model:'synthetic-main',provider_messages:[{role:'user',content:item.prompt}],memories:[{key:'explanation_style',value:'Concise, with runnable examples. '+hostile,revision:4,scope:'global',evidence:{quote:'I like concise explanations with runnable examples.'}}]}};}
  await page.route('http://127.0.0.1:8080/**',async route=>{
   const req=route.request(),u=new URL(req.url()),p=u.pathname;
   const staticFiles={'/':'index.html','/app.js':'app.js','/style.css':'style.css'};
   if(staticFiles[p])return route.fulfill({contentType:p.endsWith('.js')?'text/javascript':p.endsWith('.css')?'text/css':'text/html',body:fs.readFileSync(path.join(root,'static',staticFiles[p]),'utf8')});
   if(req.headers().authorization!=='Bearer fixture-token')return route.fulfill({status:401,json:{error:'Bearer token required'}});
   if(offline&&p.startsWith('/chat/requests/'))return route.abort('failed');
   if(p==='/memory/status')return route.fulfill({json:{active_memories:1,pending_confirmations:0,queued_jobs:1000,failed_jobs:0}});
   if(p==='/sessions')return route.fulfill({json:{sessions:[...records.values()].map(v=>({id:v.session_id,scope:v.scope,title:v.prompt,message_count:2})),has_more:false}});
   if(p.startsWith('/sessions/')){
    const sid=p.split('/')[2];const msgs=[];let seq=0;
    for(const v of records.values())if(v.session_id===sid){const d=receipt(v);msgs.push({seq:++seq,id:v.request_id,role:'user',content:v.prompt,status:d.state==='complete'?'complete':d.state==='generating'?'pending':'failed',generation_state:d.state,request_id:v.request_id});if(d.response)msgs.push({seq:++seq,id:v.request_id+'-answer',role:'assistant',content:d.response,status:'complete',request_id:v.request_id});}
    return route.fulfill({json:{scope:'global',messages:msgs,has_more:false}});
   }
   if(p==='/chat/submit'){
    submits++;const body=JSON.parse(req.postData());
    if(!records.has(body.request_id))records.set(body.request_id,{...body,final:nextState});
    if(abortSubmit){abortSubmit=false;return route.abort('failed');}
    return route.fulfill({status:202,json:receipt(records.get(body.request_id))});
   }
   if(p.startsWith('/chat/requests/')){polls++;const id=p.split('/')[3];if(!records.has(id))return route.fulfill({status:404,json:{error:'Recording receipt not found'}});return route.fulfill({json:receipt(records.get(id))});}
   return route.fulfill({status:404,json:{error:'Unmocked route '+p}});
  });
  const connect=async()=>{await page.fill('#token','fixture-token');await page.click('#authform button');await page.waitForFunction(()=>!document.getElementById('workspace').hidden);};
  const shot=async(name)=>{
   assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),name+' overflow');
   await page.screenshot({path:path.join(out,name+'.png'),fullPage:true});
   let snapshot=await page.evaluate(()=>{ const copy=document.documentElement.cloneNode(true); for(const input of document.querySelectorAll('textarea')) copy.querySelector('#'+input.id).textContent=input.value; return '<!doctype html>'+copy.outerHTML; });snapshot=snapshot.replace(/<script[^>]*>[\s\S]*?<\/script>/g,'').replace('<link rel="stylesheet" href="/style.css">','<style>'+fs.readFileSync(path.join(root,'static/style.css'),'utf8')+'</style>');
   if(name.includes('dark'))snapshot=snapshot.replace('@media(prefers-color-scheme:dark)','@media all');
   fs.writeFileSync(path.join(out,name+'.html'),snapshot);
  };
  await page.goto('http://127.0.0.1:8080');await connect();
  // Durable acknowledgement before answer; no misleading "saved answer" label.
  hold=true;await page.fill('#prompt','How should we separate chat history from memory?');await page.click('#send');
  await page.waitForFunction(()=>document.getElementById('capturestatus').textContent==='Thinking…');assert.strictEqual(await page.locator('#prompt').inputValue(),'');
  assert(!await page.evaluate(()=>JSON.stringify({...localStorage,...sessionStorage}).includes('How should we')));
  hold=false;await page.waitForFunction(()=>document.getElementById('notice').textContent.startsWith('Answer saved'));
  await page.locator('.receipt>summary').last().click();await page.waitForSelector('.receipt-memory');
  assert((await page.locator('.receipt-content').innerText()).includes('Waiting for queue space'));
  assert.strictEqual(await page.locator('.receipt-content img').count(),0);assert.strictEqual(await page.evaluate(()=>window.INJECTED),undefined);
  await shot('receipt-desktop');await page.setViewportSize({width:390,height:844});await page.emulateMedia({colorScheme:'dark'});await shot('receipt-dark-mobile');
  // Response lost AFTER the mock commits. Checking recovers without a second POST.
  await page.emulateMedia({colorScheme:'light'});await page.click('#newchat');abortSubmit=true;
  await page.fill('#prompt','Keep this even if the connection drops.');const before=submits;await page.click('#send');
  await page.waitForFunction(()=>document.getElementById('capturestatus').textContent==='Needs checking');assert.strictEqual(submits,before+1);
  assert.strictEqual(await page.locator('#prompt').inputValue(),'Keep this even if the connection drops.');
  await shot('recording-unknown-mobile');await page.click('#checkrecording');await page.waitForFunction(()=>document.getElementById('notice').textContent.startsWith('Answer saved'));assert.strictEqual(submits,before+1);assert.strictEqual(await page.locator('#prompt').inputValue(),'');
  // Reload restores only request identity, never a bearer token or raw draft; no resend.
  await page.click('#newchat');hold=true;await page.fill('#prompt','Recover after reloading this tab.');await page.click('#send');await page.waitForFunction(()=>document.getElementById('capturestatus').textContent==='Thinking…');const beforeReload=submits;
  await page.reload();assert(await page.locator('#auth').isVisible());hold=false;await connect();await page.waitForFunction(()=>document.getElementById('notice').textContent.startsWith('Answer saved'));assert.strictEqual(submits,beforeReload);
  assert(!await page.evaluate(()=>JSON.stringify({...localStorage,...sessionStorage}).includes('fixture-token')));
  // Provider failure and restart interruption retain the captured user message.
  for(const state of ['failed','interrupted']){
   nextState=state;await page.click('#newchat');await page.fill('#prompt','This message survives '+state+'.');await page.click('#send');
   await page.waitForFunction(s=>document.getElementById('capturestatus').textContent.includes(s==='failed'?'answer failed':'answer interrupted'),state);
   assert((await page.locator('#log').innerText()).includes('This message survives'));
   assert.strictEqual(await page.evaluate(()=>sessionStorage.getItem('harness_pending')),null);
  }
  // History really reopens a previous session rather than starting a new one.
  await page.setViewportSize({width:1120,height:900});await page.click('#sessionhistory>summary');await page.waitForSelector('.session-entry');await shot('history-desktop');
  await page.locator('.session-entry').first().click();await page.waitForFunction(()=>document.getElementById('log').textContent.includes('How should we'));
  assert(submits>=5);assert(polls>0);assert.deepStrictEqual(errors,[]);
  await page.click('#lock');assert.strictEqual(await page.locator('#log').innerText(),'');assert.strictEqual(await page.locator('#sessionlist').innerText(),'');
  const result={status:'passed',scope:'Mocked API only — Rust not executed',checks:['saved_before_answer','raw_prompt_not_persisted_in_browser','context_receipt','memory_backlog_does_not_hide_answer','hostile_context_as_text','desktop_mobile_dark_light_no_overflow','lost_submit_response_recovers_without_resend','reload_recovers_without_resend','token_not_persisted','provider_failure_retains_message','restart_interruption_retains_message','session_reopen','lock_clears_visible_history','no_javascript_exceptions']};
  fs.writeFileSync(path.join(out,'recording-ui-results.json'),JSON.stringify(result,null,2));console.log(JSON.stringify(result));
 } finally {await browser.close();}
})().then(()=>process.exit(0)).catch(e=>{console.error(e);process.exit(1)});
