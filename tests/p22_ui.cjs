// P22 browser contract: real DOM/focus/layout, synthetic HTTP records. No provider payload is trusted.
const {chromium} = require('playwright');
const fs = require('fs'), path = require('path'), assert = require('assert');
const root = path.resolve(__dirname,'..'), out = path.join(root,'docs/qa/p22');
fs.mkdirSync(out,{recursive:true});
(async()=>{
 const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH||chromium.executablePath(),args:['--no-sandbox']});
 try {
  const page=await browser.newPage({viewport:{width:1440,height:1000}}), errors=[];
  page.on('pageerror',e=>errors.push(String(e)));
  let version=0, expired=false, submits=0, releaseLate, holdModels=false, releaseModels, releaseInitialSessions;
  let initialRestore=true;
  const initialSessionsDelay=new Promise(resolve=>{releaseInitialSessions=resolve;});
  const modelsDelay=new Promise(resolve=>{releaseModels=resolve;});
  const delay=new Promise(resolve=>{releaseLate=resolve;});
  const long='long-provider-and-model-'.repeat(5), sentinel='PRIVATE-PAYLOAD-SENTINEL';
  await page.route('http://127.0.0.1:8080/**',async route=>{
   const req=route.request(), u=new URL(req.url()), p=u.pathname;
   const staticFiles={'/':'index.html','/api.js':'api.js','/app.js':'app.js','/style.css':'style.css'};
   if(staticFiles[p])return route.fulfill({contentType:p.endsWith('.js')?'text/javascript':p.endsWith('.css')?'text/css':'text/html',body:fs.readFileSync(path.join(root,'static',staticFiles[p]),'utf8')});
   if(p==='/auth/session')return route.fulfill({status:201,json:{session_token:'p22-session-'+(++version),expires_in:900}});
   if(expired || req.headers().authorization!=='Bearer p22-session-'+version)return route.fulfill({status:401,json:{error:'Bearer token required',code:'unauthorized',retryable:false}});
   if(p==='/models' && holdModels){await modelsDelay;return route.fulfill({status:500,json:{error:'LATE-MODEL-SECRET',code:'internal_error'}});}
   let data={};
   if(p==='/p22-active')data={active:req.headers()['x-harness-active']||null};
   else if(p==='/p22-delayed'){await delay;data={value:'LATE-STALE-RESULT'};}
   else if(p==='/health')data={ready:true,commit:'__HARNESS_BUILD_COMMIT__',schema_version:24,database:{ready:true},workers:{recording:true,extraction:true}};
   else if(p==='/memory/status')data={active_memories:0,pending_confirmations:0,queued_jobs:0,failed_jobs:0};
   else if(p==='/memory/candidates')data={candidates:[]};
   else if(p==='/sessions'){if(initialRestore)await initialSessionsDelay;data={sessions:[],has_more:false};}
   else if(p.startsWith('/sessions/'))data={messages:[],scope:'global',items:[]};
   else if(p==='/scopes')data={scopes:[{scope:'global',root_path:'/srv/'+long,permission_mode:'ask'}]};
   else if(p==='/scopes/global')data={scope:'global',root_path:'/srv/'+long,permission_mode:'ask'};
   else if(p==='/config')data={main:long,extraction:'extract',verification:'verify'};
   else if(p==='/providers')data={selected:long,providers:[{id:long,baseUrl:'https://provider.example/v1',api:'openai-completions',discovery:{type:'proxy'},version:1,selected:true,keyPresent:true,source:'environment'}]};
   else if(p==='/models')data={providerId:long,discovery:{status:'available'},data:[{id:long}],capabilities:{generation:{status:'untested'}}};
   else if(p==='/project-directories')data=u.searchParams.has('path')?{path:'/srv/projects',breadcrumbs:[{path:'/srv/projects',name:'projects'}],directories:[]}:{roots:[{path:'/srv/projects',name:'projects'}]};
   else if(p==='/jobs')data={jobs:[]};
   else if(p==='/processes')data={processes:[]};
   else if(p==='/git/state')data={trusted:false,status:{},diff_stat:{}};
   else if(p==='/chat/submit'){submits++;data={};}
   return route.fulfill({json:data});
  });
  const noOverflow=async label=>assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),label);
  const shot=async name=>{await noOverflow(name);await page.screenshot({path:path.join(out,name+'.png'),fullPage:true});};
  const connect=async()=>{expired=false;
   const restoringSessions=initialRestore?page.waitForRequest(r=>new URL(r.url()).pathname==='/sessions'):null;
   await page.fill('#token','p22-master-secret');await page.click('#auth-submit');
   if(initialRestore){await restoringSessions;assert(await page.locator('#workspace').isHidden(),'navigation is unavailable until initial restore completes');initialRestore=false;releaseInitialSessions();}
   await page.waitForFunction(()=>!document.getElementById('workspace').hidden);await page.waitForFunction(()=>document.getElementById('view-chat').getAttribute('aria-busy')!=='true');};
  await page.goto('http://127.0.0.1:8080');await shot('connect-desktop');await connect();
  const session=await page.evaluate(()=>sessionStorage.getItem('harness_session'));
  const activityHeaders=await page.evaluate(async()=>{lastInteraction=Date.now()-61000;const idle=await api('/p22-active');lastInteraction=Date.now();return {idle:idle.active,active:(await api('/p22-active')).active};});
  assert.deepStrictEqual(activityHeaders,{idle:null,active:'1'},'passive polling stops extending an idle session');
  await page.fill('#prompt','safe unsent draft in memory');
  await page.click('[data-view="settings"]');await page.waitForFunction(()=>document.getElementById('view-settings').getAttribute('aria-busy')==='false');await shot('control-center-desktop');
  await page.click('#folder-open');await page.waitForSelector('.folder-entry');
  assert.strictEqual(await page.locator('#folder-browser').getAttribute('aria-modal'),'true');
  await page.keyboard.press('Shift+Tab');
  assert(await page.evaluate(()=>document.getElementById('folder-browser').contains(document.activeElement)),'folder focus stays in dialog');
  await page.keyboard.press('Escape');assert(await page.locator('#folder-browser').isHidden());assert.strictEqual(await page.evaluate(()=>document.activeElement.id),'folder-open');
  await page.click('#provider-add');await page.fill('#providerkey','p22-provider-secret');await page.click('.provider-paste > summary');await page.fill('#providerpaste','p22-provider-paste-secret');
  await page.click('[data-view="memory"]');await page.waitForFunction(()=>document.getElementById('view-memory').getAttribute('aria-busy')==='false');
  await page.evaluate(()=>{window.lateResult='';void api('/p22-delayed').then(data=>{window.lateResult=data.value;}).catch(()=>{});});
  expired=true;await page.evaluate(()=>api('/health').catch(()=>{}));await page.waitForFunction(()=>document.getElementById('auth-title').textContent==='Session expired');
  assert.strictEqual(await page.locator('#prompt').inputValue(),'safe unsent draft in memory');
  assert.strictEqual(await page.locator('#providerkey').inputValue(),'');assert.strictEqual(await page.locator('#providerpaste').inputValue(),'');
  assert.strictEqual(await page.evaluate(()=>document.activeElement.id),'token');
  await shot('session-expired-desktop');await connect();releaseLate();await page.waitForTimeout(100);
  assert.strictEqual(await page.evaluate(()=>window.lateResult),'','old response cannot repopulate restored workspace');
  assert(await page.locator('#view-memory').isVisible());assert.strictEqual(await page.evaluate(()=>sessionStorage.getItem('harness_session')),session);
  assert.strictEqual(submits,0);assert.strictEqual(await page.locator('#prompt').inputValue(),'safe unsent draft in memory');
  const dump=await page.evaluate(()=>JSON.stringify({...localStorage,...sessionStorage}));for(const value of ['p22-master-secret','p22-session','p22-provider-secret','safe unsent draft'])assert(!dump.includes(value));
  // Deliberately malicious older/provider payloads: all private summary/preview fields are withheld.
  await page.evaluate(sentinel=>{
   const steps=['model_call','verification','compaction','subagent'].map((kind,i)=>({id:String(i),kind,status:'complete',summary:sentinel,input_preview:sentinel,output_preview:sentinel}));
   steps.push({id:'think',kind:'tool_call',tool_name:'think',status:'complete',summary:sentinel,input_preview:sentinel,output_preview:sentinel});
   steps.push({id:'read',kind:'tool_call',tool_name:'read',status:'complete',summary:'Read src/main.rs',input_preview:'{"path":"src/main.rs"}',output_preview:'<img src=x onerror="window.BAD=1">',preview_visibility:'bounded_tool'});
   document.getElementById('agent-turn').hidden=false;renderAgentSteps(steps);renderDecisionTrace({items:[{text:'Inspect recorded implementation',status:'complete'}]},steps,{state:'complete',request_id:'fixture'});
   renderVerification({status:'unverified',claims:[{status:'unverified',claim:'<script>window.BAD=1</script>',reason:'No recorded test'}]});
  },sentinel);
  assert(!(await page.locator('#agent-turn').textContent()).includes(sentinel));
  assert.strictEqual(await page.locator('#agent-turn img,#agent-turn script').count(),0);assert.strictEqual(await page.evaluate(()=>window.BAD),undefined);
  await page.click('[data-view="chat"]');await page.click('#railbtn');await page.click('[data-activity-view="decision"]');await shot('decision-trace-desktop');
  for(const width of [768,390,320]){
   await page.setViewportSize({width,height:844});
   if(width===768)await page.waitForFunction(()=>document.getElementById('rail').contains(document.activeElement)&&document.getElementById('main').inert);
   await page.keyboard.press('Escape');
   await page.click('#navtoggle');assert.strictEqual(await page.evaluate(()=>document.activeElement.id),'sidebarclose');
   await page.keyboard.press('Shift+Tab');assert(await page.evaluate(()=>document.getElementById('sidebar').contains(document.activeElement)));
   await page.keyboard.press('Escape');assert.strictEqual(await page.evaluate(()=>document.activeElement.id),'navtoggle');
   await page.click('#railbtn');assert.strictEqual(await page.locator('#rail').getAttribute('role'),'dialog');
   await page.keyboard.press('Tab');assert(await page.evaluate(()=>document.getElementById('rail').contains(document.activeElement)));
   await shot('activity-'+width);await page.keyboard.press('Escape');
   await page.click('#navtoggle');await page.click('[data-view="settings"]');await page.waitForFunction(()=>document.getElementById('view-settings').getAttribute('aria-busy')==='false');
   await page.click('#folder-open');await shot('folder-'+width);
   if(width===390){
    expired=true;await page.evaluate(()=>api('/health').catch(()=>{}));
    await page.waitForFunction(()=>document.getElementById('auth-title').textContent==='Session expired');
    assert(await page.locator('#folder-browser').isHidden());
    await connect();await page.waitForFunction(()=>document.getElementById('view-settings').getAttribute('aria-busy')==='false');
    assert.strictEqual(await page.locator('#workspace [inert]').count(),0,'re-auth removes modal inertness');
   }else await page.keyboard.press('Escape');
   await page.click('#navtoggle');await page.click('[data-view="chat"]');await noOverflow('chat '+width);
  }
  await page.emulateMedia({reducedMotion:'reduce'});await page.click('#themebtn');await shot('chat-dark-mobile');
  assert.strictEqual(await page.evaluate(()=>getComputedStyle(document.querySelector('button')).transitionDuration),'0s');
  assert(await page.evaluate(()=>document.getElementById('send').getBoundingClientRect().bottom <= document.getElementById('notice').getBoundingClientRect().top),'status notice does not cover the composer action');
  await page.fill('#prompt','manual lock clears this');
  holdModels=true;await Promise.all([page.waitForRequest(r=>new URL(r.url()).pathname==='/models'),page.evaluate(()=>{void loadModelDiscovery();})]);
  await page.click('#lock');releaseModels();await page.waitForTimeout(100);
  assert.strictEqual(await page.locator('#prompt').inputValue(),'');assert.strictEqual(await page.locator('#decision-items').textContent(),'');assert.strictEqual(await page.locator('#steps-list').textContent(),'');
  assert(!(await page.locator('#workspace').textContent()).includes(long),'manual Lock clears provider/model/project metadata');
  assert.strictEqual(await page.locator('#model-discovery-status').textContent(),'','late failures cannot repopulate locked panels');
  assert(!(await page.locator('body').textContent()).includes('LATE-MODEL-SECRET'));
  assert.deepStrictEqual(errors,[]);
  fs.writeFileSync(path.join(out,'ui-result.json'),JSON.stringify({passed:true,checks:['initial-restore-before-navigation','active-only-session-header','resize-focus-transfer','reauth-dismisses-folder-modal','lock-discards-late-error','session-reauth-same-view-conversation','no-auto-resend','draft-memory-only','late-response-discarded','provider-secrets-cleared','decision-trace-private-fields-hidden','hostile-text-inert','drawer-focus-trap-escape','folder-focus-trap-escape','320-390-768-1440-no-overflow','dark-mode','notice-keeps-composer-reachable','reduced-motion','manual-lock-clears']},null,2));
  console.log('P22 UI passed: re-auth, stale response, privacy, focus, responsive and dark-mode contracts');
 }finally{await browser.close();}
})().catch(e=>{console.error(e);process.exit(1);});
