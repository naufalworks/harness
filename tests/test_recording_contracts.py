"""Execute the actual recording schema/statements, with Python orchestration.
These are SQL contracts, NOT a substitute for Rust compilation or HTTP integration.
All fixtures synthetic; no provider calls and no legacy user database involved.
"""
import json, re, sqlite3, tempfile, unittest
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
SQL=dict(re.findall(r'pub const (\w+): &str = r#"(.*?)"#;', (ROOT/'src/recording_sql.rs').read_text(),re.S))
RUST=(ROOT/'src/recording.rs').read_text()
def statement(prefix):
    for text in re.findall(r'"((?:[^"\\]|\\.)*)"', RUST):
        text=json.loads('"'+text+'"')
        if text.startswith(prefix):return text
    raise AssertionError('Missing statement: '+prefix)
CLAIM_SELECT=statement('SELECT r.request_id,r.session_id,r.scope,r.model,m.content')
OUTBOX_SELECT=statement('SELECT o.request_id,r.scope')
HISTORY=statement('SELECT m.seq,m.id,m.role')
SESSIONS=statement('SELECT s.id,s.scope,s.created_at')
CONTEXT_HISTORY=statement('SELECT m.id,m.role,m.content')
class Recorder:
    def __init__(self,path=':memory:',fresh=True):
        self.c=sqlite3.connect(path,isolation_level=None);self.c.execute('PRAGMA foreign_keys=ON')
        self.c.execute('PRAGMA journal_mode=WAL');self.c.execute('PRAGMA synchronous=FULL')
        if fresh:
            for name in ['001_core.sql','002_recording.sql']:self.c.executescript((ROOT/'migrations'/name).read_text())
    def tx(self,fn):
        self.c.execute('BEGIN IMMEDIATE')
        try:out=fn();self.c.execute('COMMIT');return out
        except BaseException:self.c.execute('ROLLBACK');raise
    def execute(self,name,*params):return self.c.execute(SQL[name],params)
    def event(self,request,kind):self.execute('EVENT',request,kind,'now')
    def capture(self,request='r1',session='s1',prompt='I prefer Rust',signature='sig',scope='global'):
        def body():
            row=self.c.execute('SELECT signature FROM chat_receipts WHERE request_id=?',(request,)).fetchone()
            if row:return 'duplicate' if row[0]==signature else 'conflict'
            self.c.execute("INSERT INTO sessions VALUES(?,?,'now') ON CONFLICT(id) DO NOTHING",(session,scope))
            if self.c.execute('SELECT scope FROM sessions WHERE id=?',(session,)).fetchone()[0]!=scope:return 'scope_conflict'
            if self.c.execute("SELECT count(*) FROM chat_receipts WHERE session_id=? AND state IN ('captured','generating')",(session,)).fetchone()[0]:return 'busy'
            self.execute('INSERT_MESSAGE',request,session,prompt,'now')
            self.execute('INSERT_RECEIPT',request,session,scope,'synthetic-model',signature,False,'now')
            self.execute('INSERT_OUTBOX',request,'now');self.event(request,'captured');return 'saved'
        return self.tx(body)
    def claim(self):
        def body():
            row=self.c.execute(CLAIM_SELECT).fetchone()
            if row:self.execute('CLAIM',row[0],'now');self.event(row[0],'generation_started')
            return row
        return self.tx(body)
    def context(self,request='r1',value=None):
        def body():
            changed=self.execute('CONTEXT',request,json.dumps(value or {'memories':[],'provider_messages':[]}), 'now').rowcount
            if changed!=1:raise ValueError('cannot overwrite context')
            self.event(request,'context_saved')
        self.tx(body)
    def complete(self,request='r1',answer='Synthetic answer'):
        def body():
            session=self.c.execute("SELECT session_id FROM chat_receipts WHERE request_id=? AND state='generating'",(request,)).fetchone()[0]
            assert self.execute('COMPLETE_USER',request).rowcount==1
            self.execute('ANSWER',request+'-answer',session,answer,'now')
            if self.execute('COMPLETE',request,request+'-answer','now').rowcount!=1:raise ValueError('not ready')
            self.event(request,'answer_saved')
        self.tx(body)
    def fail(self,request='r1'):
        def body():
            if self.execute('FAIL',request,'provider_failed','now').rowcount:
                self.execute('FAIL_MESSAGE',request);self.event(request,'generation_failed')
        self.tx(body)
    def recover(self):
        def body():
            self.execute('RECOVER_EVENTS','restart');self.execute('RECOVER','restart');self.execute('RECOVER_MESSAGES')
        self.tx(body)
    def flush(self):
        def body():
            queued=self.c.execute("SELECT count(*) FROM jobs WHERE status IN ('pending','running')").fetchone()[0]
            rows=self.c.execute(OUTBOX_SELECT,(min(32,max(0,1000-queued)),)).fetchall()
            for request,scope,prompt in rows:
                job=request+'-job';source='chat:'+request
                self.execute('ENQUEUE',job,source,scope,source,json.dumps([{'id':request,'role':'user','content':prompt}]),0,'now')
                assert self.execute('LINK_JOB',request,job).rowcount==1
                self.event(request,'extraction_queued')
            return len(rows)
        return self.tx(body)
    def state(self,request='r1'):return self.c.execute('SELECT state FROM chat_receipts WHERE request_id=?',(request,)).fetchone()[0]
    def count(self,table):return self.c.execute('SELECT count(*) FROM '+table).fetchone()[0]
    def full_queue(self):
        self.c.executemany("INSERT INTO jobs(id,job_key,scope,source_id,payload,status,available_at,created_at) VALUES(?,?,'global','fixture','[]','pending',0,'now')",[(f'bulk-{i}',f'bulk-{i}') for i in range(1000)])
class RecordingContracts(unittest.TestCase):
    def setUp(self):self.r=Recorder();self.c=self.r.c
    def tearDown(self):self.c.close()
    def finish(self,request='r1',session='s1'):
        self.r.capture(request,session);self.r.claim();self.r.context(request);self.r.complete(request)
    def test_additive_upgrade_does_not_invent_legacy_receipts(self):
        c=sqlite3.connect(':memory:');c.executescript((ROOT/'migrations/001_core.sql').read_text());c.execute("INSERT INTO sessions VALUES('old','global','then')");c.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES('old-msg','old','user','old text','complete','then')");c.commit();c.executescript((ROOT/'migrations/002_recording.sql').read_text())
        self.assertEqual(c.execute('SELECT content FROM messages').fetchone()[0],'old text');self.assertEqual(c.execute('SELECT count(*) FROM chat_receipts').fetchone()[0],0);self.assertEqual(c.execute('PRAGMA user_version').fetchone()[0],2);c.close()
    def test_receipt_only_after_message_and_outbox_commit(self):
        self.assertEqual(self.r.capture(),'saved');self.assertEqual(self.r.count('messages'),1);self.assertEqual(self.r.count('recording_outbox'),1);self.assertEqual(self.r.state(),'captured');self.assertEqual(self.r.count('memories'),0)
    def test_failed_admission_rolls_back_all_writes(self):
        self.c.execute("CREATE TRIGGER fail_outbox BEFORE INSERT ON recording_outbox BEGIN SELECT RAISE(ABORT,'injected'); END")
        with self.assertRaises(sqlite3.IntegrityError):self.r.capture()
        for table in ['sessions','messages','chat_receipts','recording_outbox','recording_events']:self.assertEqual(self.r.count(table),0)
    def test_duplicate_submission_has_one_message_and_intent(self):
        self.r.capture();self.assertEqual(self.r.capture(),'duplicate');self.assertEqual(self.r.count('messages'),1);self.assertEqual(self.r.count('recording_outbox'),1)
    def test_same_identifier_different_content_is_conflict(self):
        self.r.capture();self.assertEqual(self.r.capture(prompt='different',signature='changed'),'conflict');self.assertEqual(self.r.count('messages'),1)
    def test_one_unfinished_turn_per_session(self):
        self.r.capture();self.assertEqual(self.r.capture('r2'),'busy');self.assertEqual(self.r.capture('r3','s3'),'saved')
    def test_same_session_cannot_change_scope(self):
        self.finish();self.assertEqual(self.r.capture('r2',scope='other'),'scope_conflict')
    def test_claim_is_once_and_ordered(self):
        self.r.capture('r1','s1');self.r.capture('r2','s2');self.assertEqual(self.r.claim()[0],'r1');self.assertEqual(self.r.claim()[0],'r2');self.assertIsNone(self.r.claim())
    def test_full_extraction_queue_cannot_undo_answer(self):
        self.r.full_queue();self.finish();self.assertEqual(self.r.state(),'complete');self.assertEqual(self.r.flush(),0);self.assertEqual(self.r.count('messages'),2);self.assertIsNone(self.c.execute('SELECT job_id FROM recording_outbox').fetchone()[0])
    def test_deferred_extraction_resumes_exactly_once(self):
        self.r.full_queue();self.finish();self.r.flush();self.c.execute("UPDATE jobs SET status='done' WHERE id='bulk-0'");self.assertEqual(self.r.flush(),1);self.assertEqual(self.r.flush(),0);self.assertEqual(self.r.count('jobs'),1001)
    def test_failed_job_insert_leaves_answer_intact_and_retriable(self):
        self.finish();self.c.execute("CREATE TRIGGER fail_job BEFORE INSERT ON jobs BEGIN SELECT RAISE(ABORT,'injected'); END")
        with self.assertRaises(sqlite3.IntegrityError):self.r.flush()
        self.assertEqual(self.r.state(),'complete');self.assertIsNone(self.c.execute('SELECT job_id FROM recording_outbox').fetchone()[0]);self.c.execute('DROP TRIGGER fail_job');self.assertEqual(self.r.flush(),1)
    def test_completion_needs_recorded_context(self):
        self.r.capture();self.r.claim()
        with self.assertRaises(ValueError):self.r.complete()
        self.assertEqual(self.r.count('messages'),1);self.assertEqual(self.c.execute('SELECT status FROM messages').fetchone()[0],'pending')
    def test_answer_save_error_rolls_back_completion_not_capture(self):
        self.r.capture();self.r.claim();self.r.context();self.c.execute("CREATE TRIGGER fail_answer BEFORE INSERT ON messages WHEN NEW.role='assistant' BEGIN SELECT RAISE(ABORT,'injected'); END")
        with self.assertRaises(sqlite3.IntegrityError):self.r.complete()
        self.assertEqual(self.r.state(),'generating');self.assertEqual(self.r.count('messages'),1)
    def test_context_snapshot_is_write_once(self):
        self.r.capture();self.r.claim();value={'memories':[{'key':'language','value':'Rust','revision':1}],'provider_messages':[{'role':'user','content':'I prefer Rust'}]};self.r.context(value=value)
        with self.assertRaises(ValueError):self.r.context(value={'changed':True})
        with self.assertRaises(sqlite3.IntegrityError):self.c.execute("UPDATE chat_receipts SET context_json='{}'")
        self.assertEqual(json.loads(self.c.execute('SELECT context_json FROM chat_receipts').fetchone()[0]),value)
    def test_rejecting_candidate_does_not_delete_recording(self):
        self.finish();self.r.flush();self.c.execute("INSERT INTO candidates VALUES('p','global','language','Rust','preference','chat:r1','{}',0,'pending','now',9999999999,NULL)");self.c.execute("UPDATE candidates SET status='rejected' WHERE id='p'");self.assertEqual(self.r.count('messages'),2);self.assertEqual(self.r.state(),'complete')
    def test_provider_failure_preserves_user_and_extraction_intent(self):
        self.r.capture();self.r.claim();self.r.context();self.r.fail();self.assertEqual(self.r.state(),'failed');self.assertEqual(self.r.count('messages'),1);self.assertEqual(self.r.flush(),1)
    def test_failed_turn_does_not_reenter_generation(self):
        self.r.capture();self.r.claim();self.r.fail();self.assertEqual(self.r.capture(),'duplicate');self.assertIsNone(self.r.claim())
    def test_restart_marks_only_started_generation_interrupted(self):
        self.r.capture('r1','s1');self.r.claim();self.r.capture('r2','s2');self.r.recover();self.r.recover();self.assertEqual(self.r.state(),'interrupted');self.assertEqual(self.r.state('r2'),'captured');self.assertEqual(self.r.claim()[0],'r2');self.assertEqual(self.c.execute("SELECT count(*) FROM recording_events WHERE kind='interrupted'").fetchone()[0],1)
    def test_restart_does_not_downgrade_completed_answer(self):
        self.finish();self.r.recover();self.assertEqual(self.r.state(),'complete')
    def test_committed_receipt_survives_close_and_reopen(self):
        with tempfile.TemporaryDirectory() as d:
            path=str(Path(d)/'db');r=Recorder(path);r.capture();r.c.close();r=Recorder(path,fresh=False);self.assertEqual(r.state(),'captured');self.assertEqual(r.c.execute('PRAGMA integrity_check').fetchone()[0],'ok');r.c.close()
    def test_message_keyset_pagination_covers_history_without_duplicates(self):
        self.c.execute("INSERT INTO sessions VALUES('s','global','now')")
        self.c.executemany("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?,'s','user','fixture','complete','now')",[(str(i),) for i in range(257)])
        ids=[];cursor=2**63-1
        while True:
            rows=self.c.execute(HISTORY,('s',cursor)).fetchall();more=len(rows)>100;rows=rows[:100];ids.extend(r[1] for r in rows)
            if not more:break
            cursor=rows[-1][0]
        self.assertEqual(len(ids),257);self.assertEqual(len(set(ids)),257)
    def test_session_pagination_does_not_strand_old_work(self):
        for i in range(63):self.c.execute("INSERT INTO sessions VALUES(?,'global','now')",(str(i),));self.c.execute("INSERT INTO messages(id,session_id,role,content,status,created_at) VALUES(?,?,'user','fixture','complete','now')",(str(i),str(i)))
        first=self.c.execute(SESSIONS,(2**63-1,)).fetchall()[:50];rest=self.c.execute(SESSIONS,(first[-1][3],)).fetchall();self.assertEqual(len(first+rest),63)
    def test_failed_history_excluded_from_model_context(self):
        self.finish();self.r.capture('r2');self.r.claim();self.r.context('r2');self.r.fail('r2');self.r.capture('r3');row=self.r.claim();events=self.c.execute(CONTEXT_HISTORY,('s1',row[-1])).fetchall();self.assertEqual({e[0] for e in events},{'r1','r1-answer'})
    def test_every_embedded_select_prepares_against_actual_schema(self):
        for text in re.findall(r'"((?:[^"\\]|\\.)*)"',RUST):
            query=json.loads('"'+text+'"')
            if query.startswith(('SELECT ','UPDATE jobs ')):
                count=max([int(n) for n in re.findall(r'\?(\d+)',query)] or [0])
                self.c.execute('EXPLAIN '+query,[None]*count)
    def test_foreign_keys_and_integrity_after_lifecycle(self):
        self.finish();self.r.flush();self.r.recover();self.assertEqual(self.c.execute('PRAGMA foreign_key_check').fetchall(),[]);self.assertEqual(self.c.execute('PRAGMA integrity_check').fetchone()[0],'ok')
if __name__=='__main__':unittest.main(verbosity=2)
