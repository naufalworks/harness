// Browser UI tests against a mocked API. Does NOT test the Rust server.
const {chromium}=require('playwright');
const fs=require('fs');const path=require('path');const assert=require('assert');
const root=path.resolve(__dirname,'..');const out=process.env.QA_DIR || path.join(root,'docs','qa');fs.mkdirSync(out,{recursive:true});
(async()=>{
 const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH||'/usr/local/bin/chromium',args:['--no-sandbox']});
 try{
  const page=await browser.newPage({viewport:{width:1120,height:900},colorScheme:'light'});const errors=[];page.on('pageerror',e=>errors.push(String(e)));
  let importCandidate=true,inlineCandidate=true,inlineData=null,failConfirm=false,chatHistory=[],receipt=null,importCalls=0,retryCalls=0,reverts=0,editCalls=0,savedConfig=null,previewCalls=0;
  let selectedProvider='environment',providerSecret=null,providerPosts=[],modelDiscoveryMode='available';
  let savedProject={root_path:null,permission_mode:'ask',diagnostics_cmd:null,max_steps:40,max_tool_bytes:400000,max_wall_seconds:900};
  const providerRows=()=>[
   {id:'environment',baseUrl:'https://startup.example/v1',api:'openai-completions',discovery:{type:'proxy'},version:1,selected:selectedProvider==='environment',keyPresent:true,source:'environment'},
   ...(providerSecret===null?[]:[{id:'ui-provider',baseUrl:'https://api.example.com/v1',api:'openai-completions',discovery:{type:'proxy'},version:providerPosts.length,selected:selectedProvider==='ui-provider',keyPresent:true,source:'saved'}])
  ];
  const malicious='<img src=x onerror="window.INJECTED=1">';
  // P16-T01 fixtures. The graph carries one recorded dependency, one row that merely
  // co-occurred (temporal proximity), and an omitted-node cursor so expansion is exercised.
  // Labels come from recorded tool names and paths, so hostile text must stay inert.
  let incidentCalls=[];
  const incidentNodes=[
   {id:'request:req-1',kind:'request',row_id:'req-1',label:'coding turn',status:'failed',path:null,tool:null,at:'2026-09-09T05:41:00Z',seq:-1,upstream:[],downstream:['permission:permit-a'],confidence:'recorded_dependency',confidence_basis:'provenance_edge',provenance:'known'},
   {id:'step:step-a',kind:'step',row_id:'step-a',label:'tool_call: edit '+malicious,status:'complete',path:null,tool:'edit',at:'2026-09-09T05:41:01Z',seq:0,upstream:[],downstream:['permission:permit-a'],confidence:'recorded_dependency',confidence_basis:'provenance_edge',provenance:'known'},
   {id:'permission:permit-a',kind:'permission',row_id:'permit-a',label:'edit: write notes.md',status:'approved',path:null,tool:'edit',at:'2026-09-09T05:41:02Z',seq:10000,upstream:['step:step-a'],downstream:[],confidence:'recorded_dependency',confidence_basis:'provenance_edge',provenance:'known'},
   {id:'step:step-b',kind:'step',row_id:'step-b',label:'tool_call: bash',status:'failed',path:null,tool:'bash',at:'2026-09-09T05:41:03Z',seq:1,upstream:[],downstream:[],confidence:'temporal_proximity',confidence_basis:'same_request_recorded_time',provenance:'unknown'},
   {id:'mutation:mut-a',kind:'mutation',row_id:'mut-a',label:'modify src/lib.rs',status:'applied',path:'src/lib.rs',tool:null,at:null,seq:15000,upstream:[],downstream:[],confidence:'unknown',confidence_basis:'no_evidence',provenance:'unknown'}
  ];
  const incidentEdges=[{id:'edge-a',source:'step:step-a',target:'permission:permit-a',relation:'depends_on',confidence:'recorded_dependency'}];
  function incidentGraph(params){
   const view=params.get('view')||'causal';
   let nodes=incidentNodes.slice();
   const tool=params.get('tool'),kind=params.get('kind'),status=params.get('status'),q=params.get('q'),relation=params.get('relation');
   let edges=relation?incidentEdges.filter(e=>e.relation===relation):incidentEdges.slice();
   if(tool)nodes=nodes.filter(n=>n.id==='request:req-1'||(n.tool||'').includes(tool));
   if(kind)nodes=nodes.filter(n=>n.id==='request:req-1'||n.kind===kind);
   if(status)nodes=nodes.filter(n=>n.id==='request:req-1'||n.status===status);
   if(q)nodes=nodes.filter(n=>n.id==='request:req-1'||`${n.label} ${n.kind} ${n.status}`.toLowerCase().includes(q.toLowerCase()));
   const visible=new Set(nodes.map(n=>n.id));
   edges=edges.filter(e=>visible.has(e.source)&&visible.has(e.target));
   // Only the unfiltered first page omits a node, so the expansion control is offered exactly
   // once and the anchored follow-up returns the complete set.
   const omit=!params.get('anchor')&&!tool&&!kind&&!status&&!q&&!relation;
   if(omit)nodes=nodes.slice(0,nodes.length-1);
   const counts={nodes:{total:incidentNodes.length,returned:nodes.length,omitted:incidentNodes.length-nodes.length},edges:{total:edges.length,returned:edges.length,omitted:0},unfiltered:{nodes:incidentNodes.length,edges:incidentEdges.length},
    confidence:{recorded_dependency:nodes.filter(n=>n.confidence==='recorded_dependency').length,temporal_proximity:nodes.filter(n=>n.confidence==='temporal_proximity').length,unknown:nodes.filter(n=>n.confidence==='unknown').length}};
   const timeline=nodes.map(n=>({node_id:n.id,kind:n.kind,row_id:n.row_id,label:n.label,status:n.status,at:n.at,seq:n.seq,confidence:n.confidence}))
    .sort((a,b)=>((a.at===null)-(b.at===null))||String(a.at).localeCompare(String(b.at)));
   return {request:{request_id:'req-1',session_id:'session',scope:'global',state:'failed',updated_at:'2026-09-09T05:41:04Z'},
    projection:'causal-neighborhood-v1',query:{view,relation,kind,status,path:params.get('path'),tool,row_id:params.get('row_id'),q,filtered:Boolean(tool||kind||status||q||relation)},
    nodes,edges,timeline,timeline_undated:timeline.filter(e=>e.at===null).length,unknown_provenance:[],
    earliest_known_break:{node_id:'step:step-b',kind:'step',row_id:'step-b',reason:'tool_failed',known:true,confidence:'recorded_dependency'},
    bounds:{max_nodes:400,max_edges:2000,max_timeline:400},truncated:{nodes:counts.nodes.omitted>0,edges:false},counts,
    confidence_labels:{recorded_dependency:'A provenance edge was recorded between these rows.',temporal_proximity:'The row occurred inside the same request and carries a recorded time, but no dependency was recorded. Co-occurrence is not causation.',unknown:'No dependency and no usable recorded time. Nothing is claimed.'},
    sanitizer:'harness-sanitize-v1',expansion_cursors:{nodes:omit?{projection:'causal-neighborhood-v1',break_node_id:'step:step-b',after_node_id:nodes[nodes.length-1].id}:null,edges:null},
    note:'Diagnostic provenance over recorded rows.'};
  }
  // P15-T05 fixtures. One searchable document and one already source-deleted, so the UI has to
  // show forget and delete-source as different states rather than one "removed" state.
  let historyDocs={
   'doc-live':{format_version:1,citation:{id:'doc-live',kind:'turn',source_id:'msg-1',revision:2,scope:'global',session_id:'session',timestamp:'2026-09-09T05:40:00Z',content_sha256:'a'.repeat(64),sanitizer:'harness-sanitize-v1'},
    title:'user turn '+malicious,indexed_at:'2026-09-09T05:40:01Z',forgotten_at:null,source_deleted_at:null,searchable:true,content_present:true,
    events:[{action:'index',revision:1,detail:'first index',created_at:'2026-09-09T05:40:01Z'}],
    note:'A forgotten entry keeps its content and revision trail and stops being returned. A source-deleted entry keeps only this record of having existed and been deleted.'}
  };
  let privacyCalls=[],exportCalls=[],historySearches=0,indexCalls=0;
  const historyHit=id=>({title:historyDocs[id].title,snippet:'We chose SQLite. '+malicious,snippet_truncated:true,rank:-1.2,citation:historyDocs[id].citation});

  // P15-T02 fixtures: the retrieval receipt explains an included and an excluded candidate. Keys
  // and reasons come from stored memory text, so hostile content must stay inert in the rail.
  const retrieval={format_version:1,request_id:'synthetic-request',scope:'global',strategy:'hybrid',embedding_model:'harness-local-hash-v1',prompt_fingerprint:'0'.repeat(64),budget_bytes:6144,included_bytes:64,considered:2,included:1,
   candidates:[
    {memory_id:'m-included',scope:'global',key:'preferred_language '+malicious,revision:1,rank:0,decision:'included',reason:'ranked_and_fit',lexical_score:2.0,semantic_score:0.11,scope_score:0.5,recency_score:0.25,usefulness_score:0.0,total_score:2.86,bytes:64},
    {memory_id:'m-excluded',scope:'global',key:'database_choice',revision:2,rank:1,decision:'excluded',reason:'category_budget',lexical_score:1.0,semantic_score:0.04,scope_score:0.5,recency_score:0.25,usefulness_score:0.0,total_score:1.79,bytes:48}
   ],
   note:'Records which memories retrieval sent, not how the model used them. '+malicious};
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
   const staticFiles={'/':'index.html','/style.css':'style.css','/api.js':'api.js','/app.js':'app.js'};
   if(staticFiles[p]){return route.fulfill({contentType:p.endsWith('.css')?'text/css':p.endsWith('.js')?'text/javascript':'text/html',body:fs.readFileSync(path.join(root,'static',staticFiles[p]),'utf8')});}
   if(p==='/auth/session'&&req.headers().authorization==='Bearer test-token')return route.fulfill({status:201,json:{session_token:'test-session',expires_in:900}});
   if(req.headers().authorization!=='Bearer test-session'){return route.fulfill({status:401,json:{error:'Bearer token required'}});}
   let result={};
   if(p==='/health')result={ready:true,commit:'__HARNESS_BUILD_COMMIT__',binary_sha256:'0'.repeat(64),schema_version:6,database:{ready:true},workers:{recording:true,extraction:true}};
   else if(p==='/memory/status')result={active_memories:importCandidate?1:2,pending_confirmations:Number(importCandidate)+Number(inlineCandidate&&inlineData),queued_jobs:0,failed_jobs:1};
   else if(p==='/scopes')result={scopes:[{scope:'global',root_path:savedProject.root_path,permission_mode:savedProject.permission_mode}]};
   else if(p==='/scopes/global'){
    if(req.method()==='GET')result=savedProject;
    else {savedProject={...savedProject,...JSON.parse(req.postData())};result={status:'saved'};}
   }
   else if(p==='/project-directories'){
    const requested=u.searchParams.get('path');
    if(!requested)result={configured:true,defaultDeny:false,roots:[{name:'workspaces',path:'/srv/workspaces'}],path:null,root:null,parent:null,breadcrumbs:[],directories:[],nextCursor:null,limit:50};
    else if(requested==='/srv/workspaces')result={configured:true,defaultDeny:false,roots:[{name:'workspaces',path:'/srv/workspaces'}],path:'/srv/workspaces',root:'/srv/workspaces',parent:null,breadcrumbs:[{name:'workspaces',path:'/srv/workspaces'}],directories:[{name:'alpha',path:'/srv/workspaces/alpha'}],nextCursor:null,limit:50};
    else if(requested==='/srv/workspaces/alpha')result={configured:true,defaultDeny:false,roots:[{name:'workspaces',path:'/srv/workspaces'}],path:'/srv/workspaces/alpha',root:'/srv/workspaces',parent:'/srv/workspaces',breadcrumbs:[{name:'workspaces',path:'/srv/workspaces'},{name:'alpha',path:'/srv/workspaces/alpha'}],directories:[],nextCursor:null,limit:50};
    else return route.fulfill({status:403,json:{error:'Directory is outside approved browse roots',code:'forbidden',retryable:false}});
   }
   else if(p==='/sessions')result={sessions:[]};
   else if(p.startsWith('/sessions/'))result={scope:'global',messages:chatHistory,has_more:false};
   else if(p.startsWith('/chat/requests/')&&p.endsWith('/steps'))result={steps:agentSteps,verification};
   else if(p.startsWith('/chat/requests/')&&p.endsWith('/incident')){incidentCalls.push(u.search);result=incidentGraph(u.searchParams);}
   else if(p.startsWith('/chat/requests/')&&p.endsWith('/retrieval'))result=retrieval;
   else if(p==='/memory/retrieval/preview'){previewCalls++;const body=JSON.parse(req.postData());result={scope:body.scope,strategy:'hybrid',candidate_id:body.candidate_id,candidate_state:'approved',rehearsed:true,before:[],after:[],added:[{memory_id:'m-preview',key:'database_choice '+malicious,scope:'global',revision:2,total_score:1.25,rank:0}],removed:[],note:'Deterministic re-run of the shipped retrieval ranking. '+malicious};}
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
   else if(p==='/providers'){
    if(req.method()==='GET')result={selected:selectedProvider,providers:providerRows()};
    else {
     const body=JSON.parse(req.postData());providerPosts.push(body);
     if(body.apiKey)providerSecret=body.apiKey;
     else assert(providerSecret,'blank-key edit must preserve an existing synthetic secret');
     result={id:body.id,baseUrl:body.baseUrl,api:body.api,discovery:body.discovery,version:providerPosts.length,selected:selectedProvider===body.id,keyPresent:true,source:'saved'};
    }
   }
   else if(p==='/providers/ui-provider/test')result={providerId:'ui-provider',version:providerPosts.length,connectionStatus:'reached_provider',discovery:{type:'proxy',status:'available',errorCode:null,modelCount:2},capabilities:{generation:{status:'untested'},tools:{status:'untested'},streaming:{status:'untested'},usage:{status:'untested'}},manualModelIdAllowed:true,data:[{id:'model-a'},{id:'model-b'}]};
   else if(p==='/providers/environment/select'){selectedProvider='environment';result={status:'selected',providerId:'environment',version:1};}
   else if(p==='/providers/ui-provider/select'){selectedProvider='ui-provider';result={status:'selected',providerId:'ui-provider',version:providerPosts.length};}
   else if(p==='/providers/ui-provider'&&req.method()==='DELETE'){providerSecret=null;selectedProvider='environment';result={status:'deleted'};}
   else if(p==='/config'){
    if(req.method()==='GET')result=savedConfig||{main:'synthetic-main',extraction:'synthetic-small',verification:'synthetic-verifier'};
    else {savedConfig=JSON.parse(req.postData());result={status:'saved'};}
   }
   else if(p==='/models'){
    if(modelDiscoveryMode==='unauthorized')return route.fulfill({status:401,json:{error:'Provider refused model discovery',code:'unauthorized',retryable:false}});
    const ids=selectedProvider==='ui-provider'?['ui-main','ui-small']:['synthetic-main','synthetic-small','synthetic-verifier',malicious];
    if(modelDiscoveryMode==='empty')result={providerId:selectedProvider,version:1,connectionStatus:'reached_provider',discovery:{type:'proxy',status:'empty',errorCode:null,modelCount:0},capabilities:{generation:{status:'untested'},tools:{status:'untested'},streaming:{status:'untested'},usage:{status:'untested'}},manualModelIdAllowed:true,data:[]};
    else if(modelDiscoveryMode==='timeout')result={providerId:selectedProvider,version:1,connectionStatus:'unreachable',discovery:{type:'proxy',status:'timeout',errorCode:'timeout',modelCount:0},capabilities:{generation:{status:'untested'},tools:{status:'untested'},streaming:{status:'untested'},usage:{status:'untested'}},manualModelIdAllowed:true,data:[]};
    else if(modelDiscoveryMode==='network')result={providerId:selectedProvider,version:1,connectionStatus:'unreachable',discovery:{type:'proxy',status:'network_error',errorCode:'network_error',modelCount:0},capabilities:{generation:{status:'untested'},tools:{status:'untested'},streaming:{status:'untested'},usage:{status:'untested'}},manualModelIdAllowed:true,data:[]};
    else result={providerId:selectedProvider,version:1,connectionStatus:'reached_provider',discovery:{type:'proxy',status:'available',errorCode:null,modelCount:ids.length},capabilities:{generation:{status:'untested'},tools:{status:'untested'},streaming:{status:'untested'},usage:{status:'untested'}},manualModelIdAllowed:true,data:ids.map(id=>({id}))};
   }
   else if(p==='/history/search'){historySearches++;result={format_version:1,scope:u.searchParams.get('scope'),session_id:u.searchParams.get('session_id'),kind:u.searchParams.get('kind'),query:u.searchParams.get('q'),hits:1,results:[historyHit('doc-live')],returned:1,suppressed:1,limit:50,sanitizer:'harness-sanitize-v1',note:'Only sanitized, not-forgotten, not-source-deleted documents are searched.'};}
   else if(p==='/history/index'){indexCalls++;result={format_version:1,indexed:1,revision_advanced:0,unchanged:3,refused:[],sanitizer:'harness-sanitize-v1'};}
   else if(p.startsWith('/history/documents/')&&p.endsWith('/privacy')){
    const id=p.split('/')[3];const action=JSON.parse(req.postData()).action;privacyCalls.push([id,action]);
    const doc=historyDocs[id];
    if(action==='forget'){doc.forgotten_at='2026-09-09T05:45:00Z';doc.searchable=false;doc.events=[...doc.events,{action:'forget',revision:2,detail:'suppressed',created_at:'2026-09-09T05:45:00Z'}];
     return route.fulfill({status:202,json:{outcome:'forgotten',note:'The entry will not be recalled or returned. Its content and revision trail are retained.'}});}
    if(action==='restore'){doc.forgotten_at=null;doc.searchable=true;doc.events=[...doc.events,{action:'restore',revision:2,detail:'suppression lifted',created_at:'2026-09-09T05:46:00Z'}];
     return route.fulfill({status:202,json:{outcome:'restored',note:'Suppression was lifted; the content was never destroyed.'}});}
    if(action==='delete_source'){doc.source_deleted_at='2026-09-09T05:47:00Z';doc.content_present=false;doc.searchable=false;doc.events=[...doc.events,{action:'delete_source',revision:2,detail:'content removed',created_at:'2026-09-09T05:47:00Z'}];
     return route.fulfill({status:202,json:{outcome:'source_deleted',note:'The source content was removed. The audited fact that this entry existed and was deleted remains.'}});}
    return route.fulfill({status:400,json:{error:'Unexpected privacy action',code:'invalid_request',retryable:false}});
   }
   else if(p.startsWith('/history/documents/')){const doc=historyDocs[p.split('/')[3]];if(!doc)return route.fulfill({status:404,json:{error:'No indexed history document with that identifier',code:'not_found',retryable:false}});result=doc;}
   else if(p==='/export/bundles'){const body=JSON.parse(req.postData());exportCalls.push(['draft',body]);
    result={format_version:1,bundle_id:'bundle-1',kind:body.kind,scope:body.scope,audience:body.audience,state:'draft',item_count:body.document_ids.length,unsanitized_items:0,
     items:body.document_ids.map(id=>({kind:'history',stable_id:id,revision:2,sanitized:true,payload:{title:historyDocs[id].title,body:'We chose SQLite. '+malicious}})),
     note:'Nothing has left yet. Review this bundle, then release it.'};}
   else if(p==='/export/bundles/bundle-1/review'){const body=JSON.parse(req.postData());exportCalls.push(['review',body]);
    if(!body.approve)result={outcome:'preview',bundle_id:'bundle-1',kind:'history',scope:'global',audience:'my other laptop',state:'draft',item_count:1,unsanitized_items:0,content_sha256:'b'.repeat(64),
     items:[{kind:'history',stable_id:'doc-live',revision:2,sanitized:true,payload:{title:historyDocs['doc-live'].title,body:'We chose SQLite. '+malicious}}],
     note:'This is what would leave. Approve with this exact content_sha256 to record the review.'};
    else if(body.content_sha256!=='b'.repeat(64))return route.fulfill({status:409,json:{error:'The export could not be reviewed as requested',code:'conflict',retryable:false}});
    else result={outcome:'reviewed',bundle_id:'bundle-1',content_sha256:'b'.repeat(64),reviewed_at:'2026-09-09T05:50:00Z',note:'The reviewed contents are pinned by this digest. Release will refuse if they change.'};}
   else if(p==='/export/bundles/bundle-1/release'){exportCalls.push(['release',null]);result={format_version:1,bundle_id:'bundle-1',state:'released'};}
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
   await page.setViewportSize({width:390,height:844});assert.strictEqual(await page.locator('#navtoggle').getAttribute('aria-expanded'),'false');await page.click('#navtoggle');assert.strictEqual(await page.locator('#navtoggle').getAttribute('aria-expanded'),'true');await page.keyboard.press('Escape');assert.strictEqual(await page.locator('#navtoggle').getAttribute('aria-expanded'),'false');await page.setViewportSize({width:1120,height:900});
  assert.strictEqual(await page.locator('#token').inputValue(),'');assert(!await page.evaluate(()=>JSON.stringify({...localStorage,...sessionStorage}).includes('test-token')));
  await page.fill('#prompt','Help me choose the next implementation step.');await page.click('#send');await page.waitForFunction(()=>document.getElementById('notice').textContent.startsWith('Answer saved'));
   assert.strictEqual(await page.locator('#project-overview-scope').innerText(),'global');assert.strictEqual(await page.locator('#project-overview-tools').innerText(),'Chat only');assert.strictEqual(await page.locator('#usage-tokens').innerText(),'18 tokens');assert((await page.locator('#usage-cost').innerText()).includes('Tokens only'));
  await page.waitForSelector('.suggestion-tray:not([hidden]) .candidate');
  // P15-T02 adds the rehearsal button between Edit and the destructive Dismiss; the exact list is
  // asserted so a silently reordered or duplicated control fails here.
  const inline=page.locator('.suggestion-tray:not([hidden]) .candidate').first();assert.deepStrictEqual(await inline.locator('.row > button').allTextContents(),['Save','Edit','Preview retrieval','Dismiss']);
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
  // P15-T02: the rail explains why each memory was or was not sent, and hostile memory text stays
  // inert. The copy must not claim the answer was caused by the included memory.
  await page.waitForFunction(()=>document.querySelectorAll('#retrieval-list .retrieval-row').length===2);
  assert.strictEqual(await page.locator('#retrieval-count').innerText(),'1 of 2 sent');
  const retrievalText=await page.locator('#agent-retrieval').innerText();
  assert(retrievalText.includes('preferred_language '+malicious),'the memory key renders as text: '+retrievalText);
  assert(retrievalText.includes('sent to the model')&&retrievalText.includes('dropped by the context budget'),'both decisions are explained: '+retrievalText);
  assert(retrievalText.includes('hybrid retrieval')&&retrievalText.includes('64 of 6144 bytes used'),'the strategy and budget are shown: '+retrievalText);
  assert.strictEqual(await page.locator('#agent-retrieval img').count(),0);assert.strictEqual(await page.evaluate(()=>window.INJECTED),undefined);
  assert.strictEqual(await page.locator('#retrieval-list .retrieval-row.included').count(),1);
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
  // P15-T02: rehearsing retrieval for a pending proposal reports the ranking delta without
  // approving anything, and its server-supplied note stays inert text.
  const proposal=page.locator('#candidates .candidate').first();
  await proposal.getByRole('button',{name:'Preview retrieval'}).click();
  await page.waitForSelector('#candidates .retrieval-preview');
  assert.strictEqual(previewCalls,1);
  const previewText=await page.locator('#candidates .retrieval-preview').first().innerText();
  assert(previewText.includes('Retrieval would change: +1 / -0'),'the delta is reported: '+previewText);
  assert(previewText.includes('database_choice '+malicious)&&previewText.includes('Deterministic re-run'),'keys and the note render as text: '+previewText);
  assert.strictEqual(await page.locator('#candidates .retrieval-preview img').count(),0);assert.strictEqual(await page.evaluate(()=>window.INJECTED),undefined);
  assert(await proposal.getByRole('button',{name:'Save'}).isEnabled(),'previewing leaves the proposal pending and approvable');
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
  await page.waitForFunction(()=>document.querySelector('#provider-list')?.textContent.includes('environment'));
  await page.waitForFunction(()=>document.querySelector('#model-discovery-status')?.textContent.includes('discovered model ID'));
  assert.strictEqual(await page.locator('#mainmodel-origin').innerText(),'Discovered exact ID');
  assert.strictEqual(await page.locator('#verificationmodel-origin').innerText(),'Discovered exact ID');
  assert.strictEqual(await page.locator('#provider-model-options option').count(),4);
  assert((await page.locator('#model-discovery-status').innerText()).includes('does not prove generation'));
  assert(!(await page.locator('#provider-list').innerText()).includes('synthetic-ui-secret'),'provider list must never display provider secrets');
  await page.click('#provider-add');
  await page.locator('.provider-paste>summary').click();
  const pastedSecret='synthetic-ui-secret';
  await page.fill('#providerpaste','ui-provider:\n  baseUrl: https://api.example.com/v1\n  apiKey: '+pastedSecret+'\n  api: openai-completions\n  discovery:\n    type: proxy');
  await page.click('#provider-parse');
  assert.strictEqual(await page.locator('#providerpaste').inputValue(),'','successful parse clears the paste field');
  assert.strictEqual(await page.locator('#providerid').inputValue(),'ui-provider');
  assert.strictEqual(await page.locator('#providerkey').inputValue(),pastedSecret);
  const browserStorage=await page.evaluate(()=>JSON.stringify({...localStorage,...sessionStorage}));
  assert(!browserStorage.includes(pastedSecret),'provider key must not enter browser storage');
  await page.click('#provider-save');
  await page.waitForFunction(()=>document.querySelector('#provider-list')?.textContent.includes('ui-provider'));
  assert.strictEqual(providerSecret,pastedSecret);
  assert.strictEqual(await page.locator('#providerkey').inputValue(),'','provider key is cleared after save');
  assert(!(await page.locator('body').innerText()).includes(pastedSecret),'provider key must not render into visible DOM');
  const savedCard=page.locator('.provider-card').filter({hasText:'ui-provider'});
  await savedCard.getByRole('button',{name:'Edit'}).click();
  assert.strictEqual(await page.locator('#providerkey').inputValue(),'','stored key is never redisplayed while editing');
  await page.click('#provider-save');
  assert.strictEqual(providerPosts.at(-1).apiKey,undefined,'blank-key edit omits apiKey so backend retains it');
  assert.strictEqual(providerSecret,pastedSecret);
  await savedCard.getByRole('button',{name:'Test discovery'}).click();
  await page.waitForFunction(()=>document.querySelector('#provider-status')?.textContent.includes('discovery available'));
  assert((await page.locator('#provider-status').innerText()).includes('remain untested'));
  await savedCard.getByRole('button',{name:'Select'}).click();
  await page.waitForFunction(()=>document.querySelector('#provider-status')?.textContent.includes('Selected provider: ui-provider'));
  await page.waitForFunction(()=>document.querySelector('#model-discovery-status')?.textContent.startsWith('ui-provider:'));
  assert.strictEqual(await page.locator('#mainmodel').inputValue(),'synthetic-main','provider switch must not silently replace role IDs');
  assert.strictEqual(await page.locator('#mainmodel-origin').innerText(),'Manual exact ID','old provider model becomes explicit manual ID on the new provider');
  assert(await page.locator('.provider-card').filter({hasText:'ui-provider'}).getByRole('button',{name:'Delete'}).isDisabled(),'selected provider cannot be deleted from UI');
  const envCard=page.locator('.provider-card').filter({hasText:'environment'});
  await envCard.getByRole('button',{name:'Select'}).click();
  await page.waitForFunction(()=>document.querySelector('#model-discovery-status')?.textContent.startsWith('environment:'));
  assert.strictEqual(await page.locator('#mainmodel').inputValue(),'synthetic-main');
  assert.strictEqual(await page.locator('#mainmodel-origin').innerText(),'Discovered exact ID');
  const removable=page.locator('.provider-card').filter({hasText:'ui-provider'});
  page.once('dialog',dialog=>dialog.accept());
  await removable.getByRole('button',{name:'Delete'}).click();
  await page.waitForFunction(()=>!document.querySelector('#provider-list')?.textContent.includes('ui-provider'));
  assert.strictEqual(providerSecret,null);
  await page.click('#provider-add');
  await page.fill('#providerpaste','{"oops":{"baseUrl":"https://api.example.com/v1","api":"openai-completions","discovery":{"type":"proxy"},"unknown":true}}');
  await page.click('#provider-parse');
  assert((await page.locator('#provider-status').innerText()).includes('unsupported field'));
  await page.click('#provider-cancel');
  assert.strictEqual(await page.locator('#verificationmodel').inputValue(),'synthetic-verifier');
  modelDiscoveryMode='empty';await page.click('#loadmodels');await page.waitForFunction(()=>document.querySelector('#model-discovery-status')?.textContent.includes('discovery is empty'));
  await page.fill('#mainmodel','manual-main-id');assert.strictEqual(await page.locator('#mainmodel-origin').innerText(),'Manual exact ID');
  modelDiscoveryMode='timeout';await page.click('#loadmodels');await page.waitForFunction(()=>document.querySelector('#model-discovery-status')?.textContent.includes('discovery timeout'));
  assert.strictEqual(await page.locator('#mainmodel').inputValue(),'manual-main-id');
  modelDiscoveryMode='network';await page.click('#loadmodels');await page.waitForFunction(()=>document.querySelector('#model-discovery-status')?.textContent.includes('network_error'));
  assert.strictEqual(await page.locator('#mainmodel').inputValue(),'manual-main-id');
  modelDiscoveryMode='unauthorized';await page.click('#loadmodels');await page.waitForFunction(()=>document.querySelector('#model-discovery-status')?.textContent.includes('discovery unavailable'));
  assert.strictEqual(await page.locator('#mainmodel').inputValue(),'manual-main-id');
  modelDiscoveryMode='available';await page.click('#loadmodels');await page.waitForFunction(()=>document.querySelector('#model-discovery-status')?.textContent.includes('discovered model ID'));
  assert.strictEqual(await page.locator('#mainmodel-origin').innerText(),'Manual exact ID');
  await page.fill('#verificationmodel','synthetic-verifier-2');await page.click('#settingsform button[type="submit"]');
  await page.waitForFunction(()=>document.getElementById('notice').textContent.includes('Model settings saved'));
  assert.strictEqual(savedConfig.verification,'synthetic-verifier-2');
  assert.strictEqual(savedConfig.main,'manual-main-id');
  await page.click('[data-view="chat"]');await page.click('[data-view="settings"]');await page.waitForFunction(()=>document.getElementById('mainmodel').value==='manual-main-id');
  assert.strictEqual(await page.locator('#mainmodel-origin').innerText(),'Manual exact ID','manual role survives settings reload');
  assert((await page.locator('#modelnames').textContent()).includes('onerror'));
  assert.strictEqual(await page.locator('#modelnames img').count(),0);
  await page.fill('#rootpath','/typed/draft');await page.selectOption('#permissionmode','auto_edit');await page.click('#folder-open');
  await page.waitForFunction(()=>document.querySelectorAll('#folder-list .folder-entry').length===1);
  await page.evaluate(()=>browseProjectDirectories('/etc'));await page.waitForFunction(()=>document.querySelector('#folder-browser-status')?.textContent.includes('Could not browse'));
  assert.strictEqual(await page.locator('#rootpath').inputValue(),'/typed/draft');assert.strictEqual(await page.locator('#permissionmode').inputValue(),'auto_edit');
  await page.click('#folder-cancel');assert.strictEqual(await page.locator('#rootpath').inputValue(),'/typed/draft');assert.strictEqual(await page.locator('#permissionmode').inputValue(),'auto_edit');
  await page.click('#folder-open');await page.waitForFunction(()=>document.querySelectorAll('#folder-list .folder-entry').length===1);await page.locator('#folder-list .folder-entry').first().click();
  await page.waitForFunction(()=>document.querySelector('#folder-current')?.textContent==='/srv/workspaces');await page.locator('#folder-list .folder-entry').filter({hasText:'alpha'}).click();
  await page.waitForFunction(()=>document.querySelector('#folder-current')?.textContent==='/srv/workspaces/alpha');await page.click('#folder-use');
  assert.strictEqual(await page.locator('#rootpath').inputValue(),'/srv/workspaces/alpha');assert.strictEqual(await page.locator('#permissionmode').inputValue(),'auto_edit','folder selection must not alter permission mode');
  if(await page.locator('#rail').isVisible())await page.click('#railclose');
  await page.click('#folder-open');await page.waitForFunction(()=>!document.querySelector('#folder-browser').hidden);await page.setViewportSize({width:390,height:844});await shot('settings-folder-mobile');await page.setViewportSize({width:1120,height:900});await page.click('#folder-cancel');
  await shot('settings-desktop');
  if(!await page.locator('#rail').isVisible())await page.click('#railbtn');
  // ---------------- P16-T01: incident search, timeline and expansion ----------------
  await page.click('[data-view="chat"]');
  await page.waitForSelector('#agent-incident:not([hidden]) .incident-node');
  // Confidence labels must distinguish a recorded dependency from mere co-occurrence, and the
  // panel must count them rather than presenting one blended "provenance" number.
  const confidenceText=await page.locator('#incident-confidence').innerText();
  assert(/recorded dependency/.test(confidenceText)&&/temporal proximity only/.test(confidenceText)&&/unknown/.test(confidenceText),'all three confidence classes are counted: '+confidenceText);
  assert.strictEqual(await page.locator('.incident-node[data-confidence="temporal_proximity"]').count(),1);
  assert.strictEqual(await page.locator('.incident-node[data-confidence="recorded_dependency"]').count(),3);
  // Hostile recorded tool text renders as text in the node label, never as markup.
  const nodesText=await page.locator('#incident-nodes').innerText();
  assert(nodesText.includes('tool_call: edit '+malicious),'the node label renders as text: '+nodesText);
  assert.strictEqual(await page.locator('#incident-nodes img').count(),0);
  assert.strictEqual(await page.evaluate(()=>window.INJECTED),undefined);
  // Selecting the proximity-only row must say co-occurrence is not causation.
  await page.locator('.incident-node[data-confidence="temporal_proximity"]').click();
  const detailText=await page.locator('#incident-detail').innerText();
  assert(detailText.includes('temporal proximity only')&&detailText.includes('Co-occurrence is not causation'),'the detail refuses to overclaim: '+detailText);
  // Search is server-side: the request carries the query, and the result narrows.
  await page.fill('#incident-search','bash');await page.dispatchEvent('#incident-search','change');
  await page.waitForFunction(()=>document.querySelectorAll('#incident-nodes .incident-node').length===2);
  assert(incidentCalls.some(search=>search.includes('q=bash')),'the query reached the server: '+incidentCalls.join(' '));
  await page.fill('#incident-search','');await page.dispatchEvent('#incident-search','change');
  await page.waitForFunction(()=>document.querySelectorAll('#incident-nodes .incident-node').length===4);
  // Filtering by node kind and by recorded status both go to the server too.
  await page.selectOption('#incident-kind','permission');
  await page.waitForFunction(()=>document.querySelectorAll('#incident-nodes .incident-node').length===2);
  assert(incidentCalls.some(search=>search.includes('kind=permission')));
  await page.selectOption('#incident-kind','all');
  await page.fill('#incident-status','failed');await page.dispatchEvent('#incident-status','change');
  // The request frame plus the one failed step; the frame always survives a filter.
  await page.waitForFunction(()=>document.querySelectorAll('#incident-nodes .incident-node').length===2);
  assert(incidentCalls.some(search=>search.includes('status=failed')));
  await page.fill('#incident-status','');await page.dispatchEvent('#incident-status','change');
  await page.waitForFunction(()=>document.querySelectorAll('#incident-nodes .incident-node').length===4);
  // Bounded expansion: the cursor is passed back verbatim, never reconstructed.
  assert(!await page.locator('#incident-expand').isHidden(),'an omitted node offers expansion');
  await page.click('#incident-expand');
  await page.waitForFunction(()=>document.querySelectorAll('#incident-nodes .incident-node').length===5);
  assert(incidentCalls.some(search=>search.includes('anchor=')&&decodeURIComponent(search).includes('causal-neighborhood-v1')),'the opaque anchor was returned unchanged: '+incidentCalls.join(' '));
  assert(await page.locator('#incident-expand').isHidden(),'a complete page stops offering expansion');
  // The chronological view is a real second view over the same projection, and an undated row
  // is reported as undated instead of being placed by guesswork.
  await page.click('#incident-view-chronological');
  await page.waitForSelector('#incident-timeline:not([hidden]) .incident-timeline-row');
  assert(await page.locator('#incident-nodes').isHidden(),'the causal list yields to the timeline');
  assert(incidentCalls.some(search=>search.includes('view=chronological')));
  // Switching view drops the expansion anchor on purpose: a page from the causal ordering is
  // not a page of the chronological one. Expand again to reach the undated row.
  await page.click('#incident-expand');
  await page.waitForFunction(()=>document.querySelectorAll('#incident-timeline .incident-timeline-row').length===5);
  const timelineText=await page.locator('#incident-timeline').innerText();
  assert(timelineText.includes('no recorded time')&&/carry no recorded time/.test(timelineText),'undated rows are named, not ordered by guess: '+timelineText);
  assert.strictEqual(await page.locator('#incident-timeline img').count(),0);
  await shot('incident-timeline-desktop');
  await page.click('#incident-view-causal');
  await page.waitForSelector('#incident-nodes:not([hidden]) .incident-node');

  // ---------------- P15-T05: history search, privacy boundary, export review ----------------
  await page.click('[data-view="history"]');
  await page.fill('#history-query','SQLite');
  await page.click('#history-search-button');
  await page.waitForSelector('#history-results .history-hit');
  assert.strictEqual(historySearches,1);
  const hitText=await page.locator('#history-results').innerText();
  assert(hitText.includes('We chose SQLite. '+malicious),'the snippet renders as text: '+hitText);
  assert.strictEqual(await page.locator('#history-results img').count(),0);
  assert.strictEqual(await page.evaluate(()=>window.INJECTED),undefined);
  // Every hit carries its citation, field by field, including the revision and checksum.
  await page.locator('#history-results .history-citation summary').click();
  // The grid uppercases its labels in CSS, so the check is on the rendered text case-insensitively.
  const citationText=(await page.locator('#history-results .citation-grid').innerText()).toLowerCase();
  for(const field of ['revision','checksum','sanitizer','source row'])assert(citationText.includes(field),'citation shows '+field+': '+citationText);
  assert(!hitText.includes('would have changed'),'search must not claim an answer would have changed');
  const summaryText=await page.locator('#history-summary').innerText();
  assert(summaryText.includes('suppressed by the read-time sanitizer gate'),'the suppressed count is surfaced: '+summaryText);
  // Forget and delete-source must read as two different operations, not one.
  await page.locator('#history-results .history-hit button.secondary').click();
  await page.waitForSelector('#history-forget');
  assert.strictEqual(await page.locator('#history-forget').innerText(),'Forget entry');
  assert.strictEqual(await page.locator('#history-delete-source').innerText(),'Delete source content');
  const forgetCopy=await page.locator('.privacy-op.reversible').innerText();
  const deleteCopy=await page.locator('.privacy-op.destructive').innerText();
  assert(forgetCopy.includes('reversible')&&forgetCopy.includes('kept'),'forget is described as reversible suppression: '+forgetCopy);
  assert(deleteCopy.includes('cannot be undone')&&deleteCopy.includes('different operation'),'delete-source is described as destructive and distinct: '+deleteCopy);
  assert(await page.locator('.privacy-op.destructive #history-delete-source').evaluate(el=>el.classList.contains('danger')),'the destructive control is styled apart');
  assert(!deleteCopy.toLowerCase().includes('reversible'),'delete-source must not borrow forget\u2019s promise');
  assert((await page.locator('#history-privacy .history-badge').innerText()).includes('searchable'));
  assert((await page.locator('#history-privacy').innerText()).includes('content retained'));
  await shot('history-privacy-desktop');
  // Forget keeps content and offers Restore; the audit trail records the act.
  await page.click('#history-forget');
  await page.waitForFunction(()=>document.getElementById('notice').textContent.includes('content and revision trail are retained'));
  await page.waitForSelector('#history-privacy .history-badge.forgotten');
  assert.strictEqual(await page.locator('#history-forget').innerText(),'Restore entry');
  assert((await page.locator('#history-privacy').innerText()).includes('content retained'),'a forget must not read as a content removal');
  assert((await page.locator('#history-audit').innerText()).includes('forget'));
  await page.click('#history-forget');
  await page.waitForSelector('#history-privacy .history-badge.live');
  assert.deepStrictEqual(privacyCalls,[['doc-live','forget'],['doc-live','restore']]);
  // Source delete is confirmed separately and then reads as content removed, not suppressed.
  page.once('dialog',dialog=>{assert(dialog.message().includes('Forget is reversible; this is not'));dialog.accept();});
  await page.click('#history-delete-source');
  await page.waitForSelector('#history-privacy .history-badge.deleted');
  assert((await page.locator('#history-privacy').innerText()).includes('content removed'));
  assert(await page.locator('#history-delete-source').isDisabled());
  assert(await page.locator('#history-forget').isDisabled(),'a source-deleted entry has nothing left to forget');
  assert.deepStrictEqual(privacyCalls[2],['doc-live','delete_source']);
  // Export shows what would leave, with its digest, before any release control exists.
  await page.fill('#export-audience','my other laptop');
  await page.check('#export-consent');
  await page.click('#export-draft');
  await page.waitForSelector('#export-preview:not([hidden]) .export-items li');
  const exportText=await page.locator('#export-preview').innerText();
  assert(exportText.includes('This is what would leave')&&exportText.includes('b'.repeat(64)),'the draft names its exact contents and digest: '+exportText);
  assert(exportText.includes('We chose SQLite. '+malicious)||exportText.includes('user turn '+malicious),'the item body renders as text');
  assert.strictEqual(await page.locator('#export-preview img').count(),0);
  assert.strictEqual(await page.evaluate(()=>window.INJECTED),undefined);
  assert.strictEqual(await page.locator('#export-release').count(),0,'nothing may be releasable before it is reviewed');
  await shot('export-review-desktop');
  await page.click('#export-approve');
  await page.waitForSelector('#export-release');
  page.once('dialog',dialog=>{assert(dialog.message().includes('will leave this machine'));dialog.accept();});
  await page.click('#export-release');
  await page.waitForFunction(()=>document.getElementById('notice').textContent.includes('recomputed and matched'));
  assert(( await page.locator('#export-preview').innerText()).includes('This left the machine'));
  assert.deepStrictEqual(exportCalls.map(call=>call[0]),['draft','review','review','release']);
  assert.strictEqual(exportCalls[2][1].content_sha256,'b'.repeat(64),'approval pins the digest the reviewer was shown');
  assert.strictEqual(indexCalls,0);
  await page.click('#history-index');
  await page.waitForFunction(()=>document.getElementById('notice').textContent.includes('Index refreshed'));
  assert.strictEqual(indexCalls,1);
  await page.setViewportSize({width:390,height:844});await shot('history-privacy-mobile');await page.setViewportSize({width:1120,height:900});

  await page.click('[data-view="settings"]');
  // P21-T01: configuration status is visible without promising controls that do not exist.
  await page.locator('#p21-capabilities>summary').click();
  const coverageText = await page.locator('#p21-capabilities').innerText();
  for (const phrase of ['Available now', 'Not yet available in the UI', 'Advanced API-only operations', 'model\'s private reasoning']) {
    assert(coverageText.includes(phrase), 'capability inventory omits: ' + phrase);
  }
  assert(coverageText.includes('allowlisted server folder chooser'));
  assert(coverageText.includes('custom provider management'));
  assert(coverageText.includes('write-only'));
  assert.strictEqual(await page.locator('#p21-capabilities input').count(), 0, 'inventory must not contain inert configuration fields');
  await page.click('#lock');assert(await page.locator('#workspace').isHidden());assert.strictEqual(await page.locator('#candidates').innerText(),'');
  assert.strictEqual(await page.locator('#verificationmodel').inputValue(),'');
  assert.strictEqual(await page.locator('#providerkey').inputValue(),'');
  assert.strictEqual(await page.locator('#providerpaste').inputValue(),'');
  assert.deepStrictEqual(errors,[]);
  const result={status:'passed',scope:'Mocked API browser checks; Rust server not executed',checks:['connect','keyboard_drawer_escape','token_not_persisted','conversation','inline_suggestion_tray','suggestion_edit','suggestion_dismiss','diff_card_rendered','diff_card_revert','diff_card_create_revert','verification_badge','verification_claim_text_inert','verification_model_setting','retrieval_receipt_panel','retrieval_receipt_text_inert','retrieval_preview_rehearsal','incident_confidence_labels','incident_temporal_proximity_not_causation','incident_server_side_search','incident_kind_status_filters','incident_bounded_expansion_cursor','incident_chronological_timeline','history_search_citations','history_forget_distinct_from_source_delete','history_forget_retains_content','history_source_delete_confirmed_and_irreversible','history_privacy_audit_trail','export_preview_before_release','export_digest_pinned_at_review','import_inbox_only','memory_evidence','html_injection_rendered_as_text','failed_approval_recoverable','suggestion_save','import','job_retry','model_settings','project_dashboard','mobile_overflow','dark_mode','lock','no_javascript_exceptions']};
  fs.writeFileSync(path.join(out,'ui-results.json'),JSON.stringify(result,null,2));console.log(JSON.stringify(result));
 }finally{await browser.close();}
})().then(()=>process.exit(0)).catch(e=>{console.error(e);process.exit(1)});
