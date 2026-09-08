// Shared executable statements: offline contract tests read these exact strings.
pub const INSERT_MESSAGE: &str = r#"INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?1,?2,'user',?3,'pending',?4)"#;
pub const INSERT_RECEIPT: &str = r#"INSERT INTO chat_receipts(request_id,session_id,scope,model,signature,redacted,state,captured_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,'captured',?7,?7)"#;
pub const INSERT_OUTBOX: &str = r#"INSERT INTO recording_outbox(request_id,created_at) VALUES(?1,?2)"#;
pub const EVENT: &str = r#"INSERT INTO recording_events(request_id,kind,created_at) VALUES(?1,?2,?3)"#;
pub const CLAIM: &str = r#"UPDATE chat_receipts SET state='generating',updated_at=?2 WHERE request_id=?1 AND state='captured'"#;
pub const CONTEXT: &str = r#"UPDATE chat_receipts SET context_json=?2,updated_at=?3 WHERE request_id=?1 AND state='generating' AND context_json IS NULL"#;
pub const COMPLETE_USER: &str = r#"UPDATE messages SET status='complete' WHERE id=?1 AND status='pending'"#;
pub const ANSWER: &str = r#"INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?1,?2,'assistant',?3,'complete',?4)"#;
pub const COMPLETE: &str = r#"UPDATE chat_receipts SET state='complete',answer_id=?2,updated_at=?3 WHERE request_id=?1 AND state='generating' AND context_json IS NOT NULL"#;
pub const FAIL: &str = r#"UPDATE chat_receipts SET state='failed',error_code=?2,updated_at=?3 WHERE request_id=?1 AND state='generating'"#;
pub const FAIL_MESSAGE: &str = r#"UPDATE messages SET status='failed' WHERE id=?1 AND status='pending'"#;
pub const ENQUEUE: &str = r#"INSERT INTO jobs(id,job_key,scope,source_id,payload,status,available_at,created_at) VALUES(?1,?2,?3,?4,?5,'pending',?6,?7)"#;
pub const LINK_JOB: &str = r#"UPDATE recording_outbox SET job_id=?2 WHERE request_id=?1 AND job_id IS NULL"#;
pub const RECOVER_EVENTS: &str = r#"INSERT INTO recording_events(request_id,kind,created_at) SELECT request_id,'interrupted',?1 FROM chat_receipts WHERE state='generating'"#;
pub const RECOVER: &str = r#"UPDATE chat_receipts SET state='interrupted',error_code='process_restarted',updated_at=?1 WHERE state='generating'"#;
pub const RECOVER_MESSAGES: &str = r#"UPDATE messages SET status='failed' WHERE status='pending' AND NOT EXISTS(SELECT 1 FROM chat_receipts r WHERE r.request_id=messages.id AND r.state='captured')"#;
