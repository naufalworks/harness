// Agentic-turn statements (schema 003+, including provenance in 006). Offline contract tests execute these exact strings
// against the migrated schema: tests/test_agentic_sql.py.
pub const SCOPE_GET: &str = r#"SELECT scope,root_path,permission_mode,diagnostics_cmd,max_steps,max_tool_bytes,max_wall_seconds,created_at,updated_at FROM scopes WHERE scope=?1"#;
pub const SCOPE_UPSERT: &str = r#"INSERT INTO scopes(scope,root_path,permission_mode,diagnostics_cmd,max_steps,max_tool_bytes,max_wall_seconds,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8) ON CONFLICT(scope) DO UPDATE SET root_path=excluded.root_path,permission_mode=excluded.permission_mode,diagnostics_cmd=excluded.diagnostics_cmd,max_steps=excluded.max_steps,max_tool_bytes=excluded.max_tool_bytes,max_wall_seconds=excluded.max_wall_seconds,updated_at=excluded.updated_at"#;
// P1-T15: every configured scope, so the UI can offer the ones that exist instead of asking the
// user to guess a name. Same column order as SCOPE_GET; a fresh install returns no rows.
pub const SCOPES_LIST: &str = r#"SELECT scope,root_path,permission_mode,diagnostics_cmd,max_steps,max_tool_bytes,max_wall_seconds,created_at,updated_at FROM scopes ORDER BY scope"#;

// P5-T03: `parent_step_id` (?3) is NULL for every step the main loop owns and carries the
// `task` tool-call step for the steps a sub-agent runs, so one request stays one ordered list.
pub const STEP_BEGIN: &str = r#"INSERT INTO turn_steps(id,request_id,parent_step_id,seq,kind,status,tool_name,tool_call_id,input_json,started_at) VALUES(?1,?2,?3,?4,?5,'running',?6,?7,?8,?9)"#;
// The read side of that tree. No Rust caller needs it yet — the loop writes the parent id and
// projects a request's steps with STEPS_LIST — so it is kept here as the documented statement for
// nesting a sub-agent's steps, and tests/test_agentic_sql.py executes it against the real schema.
#[allow(dead_code)]
pub const STEPS_OF_PARENT: &str = r#"SELECT id,seq,kind,status,tool_name,error_code FROM turn_steps WHERE parent_step_id=?1 ORDER BY seq"#;
pub const STEP_FINISH: &str = r#"UPDATE turn_steps SET status=?2,output_json=?3,output_bytes=?4,truncated=?5,tokens_in=?6,tokens_out=?7,error_code=?8,finished_at=?9 WHERE id=?1 AND status='running'"#;
pub const STEP_NEXT_SEQ: &str = r#"SELECT COALESCE(MAX(seq),-1)+1 FROM turn_steps WHERE request_id=?1"#;
pub const STEPS_LIST: &str = r#"SELECT id,seq,kind,status,tool_name,tool_call_id,substr(input_json,1,2048),substr(output_json,1,2048),output_bytes,truncated,tokens_in,tokens_out,error_code,started_at,finished_at FROM turn_steps WHERE request_id=?1 ORDER BY seq"#;
pub const VERIFICATION_LATEST: &str = r#"SELECT id,status,substr(output_json,1,32768),error_code,finished_at,length(COALESCE(output_json,''))>32768 FROM turn_steps WHERE request_id=?1 AND kind='verification' ORDER BY seq DESC LIMIT 1"#;

pub const EVENT: &str = r#"INSERT INTO activity_events(request_id,session_id,step_id,kind,payload_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)"#;
pub const EVENTS_AFTER: &str = r#"SELECT seq,request_id,step_id,kind,payload_json,created_at FROM activity_events WHERE session_id=?1 AND seq>?2 ORDER BY seq LIMIT 200"#;

pub const PERMISSION_CREATE: &str = r#"INSERT INTO permission_requests(id,request_id,step_id,tool_name,summary,args_json,status,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,'pending',?7,?8)"#;
pub const PERMISSION_GET: &str = r#"SELECT p.id,p.request_id,p.step_id,p.tool_name,p.summary,p.args_json,p.status,p.created_at,p.expires_at,p.resolved_at,r.scope FROM permission_requests p JOIN chat_receipts r ON r.request_id=p.request_id WHERE p.id=?1"#;
pub const PERMISSION_RESOLVE: &str = r#"UPDATE permission_requests SET status=?2,resolved_at=?3 WHERE id=?1 AND status='pending'"#;
pub const PERMISSION_STATUS: &str = r#"SELECT status FROM permission_requests WHERE id=?1"#;
pub const PERMISSIONS_PENDING: &str = r#"SELECT p.id,p.request_id,p.step_id,p.tool_name,p.summary,p.args_json,p.created_at,p.expires_at FROM permission_requests p JOIN chat_receipts r ON r.request_id=p.request_id WHERE p.status='pending' AND r.scope=?1 ORDER BY p.created_at"#;
pub const PERMISSION_EXPIRE: &str = r#"UPDATE permission_requests SET status='expired',resolved_at=?2 WHERE id=?1 AND status='pending'"#;

pub const FILE_CHANGE: &str = r#"INSERT INTO file_changes(id,request_id,step_id,path,action,before_hash,after_hash,diff,applied,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"#;
pub const FILE_CHANGES_LIST: &str = r#"SELECT id,step_id,path,action,before_hash,after_hash,diff,applied,reverted_at,created_at FROM file_changes WHERE request_id=?1 ORDER BY created_at"#;
// P2-T03: one change by id, carrying the scope and session of the turn that made it. The project
// root is resolved from this row and never from the caller, so a revert cannot be aimed at
// another project. `reverted_at IS NULL` guards the update: a double-clicked Revert updates no
// rows instead of recording a second undo of the same change.
pub const FILE_CHANGE_GET: &str = r#"SELECT f.id,f.request_id,f.step_id,f.path,f.action,f.before_hash,f.after_hash,f.diff,f.applied,f.reverted_at,r.scope,r.session_id FROM file_changes f JOIN chat_receipts r ON r.request_id=f.request_id WHERE f.id=?1"#;
pub const FILE_CHANGE_REVERTED: &str = r#"UPDATE file_changes SET reverted_at=?2 WHERE id=?1 AND reverted_at IS NULL"#;

// P8-T01: edges carry row identities only. Endpoint existence and request/scope ownership are
// enforced by migration 006's trigger, so a caller cannot create a plausible-looking orphan.
pub const PROVENANCE_EDGE_INSERT: &str = r#"INSERT INTO provenance_edges(id,request_id,source_kind,source_id,relation,target_kind,target_id,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)"#;
pub const PROVENANCE_EDGES_LIST: &str = r#"SELECT id,source_kind,source_id,relation,target_kind,target_id,created_at FROM provenance_edges WHERE request_id=?1 ORDER BY created_at,id LIMIT 2000"#;

pub const PLAN_CLEAR: &str = r#"DELETE FROM plan_items WHERE session_id=?1"#;
pub const PLAN_INSERT: &str = r#"INSERT INTO plan_items(id,session_id,seq,text,status,updated_at) VALUES(?1,?2,?3,?4,?5,?6)"#;
pub const PLAN_LIST: &str = r#"SELECT seq,text,status,updated_at FROM plan_items WHERE session_id=?1 ORDER BY seq"#;

pub const SESSION_OF_REQUEST: &str = r#"SELECT session_id FROM chat_receipts WHERE request_id=?1"#;

// Recovery (runs inside recording::recover's transaction, before the receipt is interrupted).
pub const RECOVER_STEPS: &str = r#"UPDATE turn_steps SET status='interrupted',finished_at=?1 WHERE status='running'"#;
pub const RECOVER_PERMISSIONS: &str = r#"UPDATE permission_requests SET status='expired',resolved_at=?1 WHERE status='pending'"#;
pub const RECOVER_ACTIVITY: &str = r#"INSERT INTO activity_events(request_id,session_id,step_id,kind,payload_json,created_at) SELECT r.request_id,r.session_id,(SELECT s.id FROM turn_steps s WHERE s.request_id=r.request_id AND s.status='interrupted' AND s.finished_at=?1 ORDER BY s.seq DESC LIMIT 1),'interrupted','{}',?1 FROM chat_receipts r WHERE r.state='generating'"#;
pub const RECOVER_PROVENANCE: &str = r#"INSERT INTO provenance_edges(id,request_id,source_kind,source_id,relation,target_kind,target_id,created_at) SELECT lower(hex(randomblob(16))),a.request_id,'step',a.step_id,'triggers','recovery',CAST(a.seq AS TEXT),?1 FROM activity_events a WHERE a.kind='interrupted' AND a.created_at=?1 AND a.step_id IS NOT NULL"#;
