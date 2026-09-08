"""Offline SQLite contract tests. These execute the shipped schema and mutation SQL,
with a Python transaction driver mirroring Rust orchestration. They do not compile
or execute the Rust implementation; the Rust/HTTP release gates remain separate.
"""
import importlib.util,json,re,sqlite3,sys,tempfile,time,unittest,uuid
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/'scripts'))
from backup import backup
from migrate_legacy import migrate
SCHEMA=(ROOT/'migrations/001_core.sql').read_text()
RUST=(ROOT/'src/storage.rs').read_text()
def sql_start(prefix):
    for raw in re.findall(r'(?:tx|c)\.execute\("((?:[^"\\]|\\.)*)"',RUST):
        sql=json.loads('"'+raw+'"')
        if sql.startswith(prefix):return sql
    raise AssertionError('SQL not found: '+prefix)
UPSERT=sql_start('INSERT INTO memories(')
REVISION=sql_start('INSERT INTO memory_revisions(')
APPROVE=sql_start("UPDATE candidates SET status='approved'")
RECALL=re.search(r'c\.prepare\("(SELECT m.id,m.scope.*?)"\)',RUST).group(1)

def connect(path=':memory:'):
    c=sqlite3.connect(path,isolation_level=None,timeout=5);c.execute('PRAGMA foreign_keys=ON');c.executescript(SCHEMA);return c

def proposal(c,identifier='p1',scope='global',key='language',value='Rust',revision=0,source='s1'):
    c.execute("INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at) VALUES(?,?,?,?,?,?,?,?,'pending','2026-09-08',?)",(identifier,scope,key,value,'preference',source,json.dumps({'quote':'I prefer '+value}),revision,int(time.time())+3600))

def resolve(c,identifier,scope='global',confirm=True):
    c.execute('BEGIN IMMEDIATE')
    try:
        row=c.execute('SELECT key,value,category,expected_revision,status FROM candidates WHERE id=? AND scope=?',(identifier,scope)).fetchone()
        if not row:c.execute('ROLLBACK');return 'not_found'
        key,value,category,expected,status=row
        if status!='pending':c.execute('ROLLBACK');return 'already_resolved'
        if not confirm:
            c.execute("UPDATE candidates SET status='rejected' WHERE id=?",(identifier,));c.execute('COMMIT');return 'rejected'
        current=c.execute('SELECT id,revision,value FROM memories WHERE scope=? AND key=?',(scope,key)).fetchone()
        if (current[1] if current else 0)!=expected:
            c.execute("UPDATE candidates SET status='conflict' WHERE id=?",(identifier,));c.execute('COMMIT');return 'conflict'
        memory=current[0] if current else str(uuid.uuid4());old=current[2] if current else None
        c.execute(UPSERT,(memory,scope,key,value,category,expected+1,identifier,'2026-09-08'))
        c.execute(REVISION,(str(uuid.uuid4()),memory,expected+1,old,value,identifier,'2026-09-08'))
        c.execute(APPROVE,('2026-09-08',identifier));c.execute('COMMIT');return 'approved'
    except Exception:c.execute('ROLLBACK');raise

class Contracts(unittest.TestCase):
    def setUp(self):self.c=connect()
    def tearDown(self):self.c.close()
    def test_schema_version_and_integrity(self):
        self.assertEqual(self.c.execute('PRAGMA user_version').fetchone()[0],1);self.assertEqual(self.c.execute('PRAGMA integrity_check').fetchone()[0],'ok')
    def test_candidates_do_not_become_active(self):
        proposal(self.c);self.assertEqual(self.c.execute('SELECT count(*) FROM memories').fetchone()[0],0)
    def test_sensitive_category_is_rejected_by_schema(self):
        proposal(self.c)
        with self.assertRaises(sqlite3.IntegrityError):self.c.execute("UPDATE candidates SET category='credential'")
    def test_approval_and_repeat_resolution(self):
        proposal(self.c);self.assertEqual(resolve(self.c,'p1'),'approved');self.assertEqual(resolve(self.c,'p1',confirm=False),'already_resolved');self.assertEqual(self.c.execute('SELECT count(*) FROM memory_revisions').fetchone()[0],1)
    def test_reject_does_not_write_memory(self):
        proposal(self.c);self.assertEqual(resolve(self.c,'p1',confirm=False),'rejected');self.assertEqual(self.c.execute('SELECT count(*) FROM memories').fetchone()[0],0)
    def test_wrong_scope_cannot_resolve(self):
        proposal(self.c);self.assertEqual(resolve(self.c,'p1',scope='another'),'not_found')
    def test_failure_rolls_back_memory_and_approval(self):
        proposal(self.c);self.c.execute("CREATE TRIGGER inject_failure BEFORE INSERT ON memory_revisions BEGIN SELECT RAISE(ABORT,'synthetic failure'); END")
        with self.assertRaises(sqlite3.IntegrityError):resolve(self.c,'p1')
        self.assertEqual(self.c.execute('SELECT status FROM candidates').fetchone()[0],'pending');self.assertEqual(self.c.execute('SELECT count(*) FROM memories').fetchone()[0],0)
    def test_stale_proposal_cannot_overwrite(self):
        proposal(self.c);proposal(self.c,'p2',value='Python',source='s2');resolve(self.c,'p1');self.assertEqual(resolve(self.c,'p2'),'conflict');self.assertEqual(self.c.execute('SELECT value FROM memories').fetchone()[0],'Rust')
    def test_new_revision_updates_fts_and_keeps_history(self):
        proposal(self.c,value='Rust');resolve(self.c,'p1');proposal(self.c,'p2',value='Python',revision=1,source='s2');resolve(self.c,'p2')
        self.assertEqual(self.c.execute("SELECT count(*) FROM memory_fts WHERE memory_fts MATCH 'Rust'").fetchone()[0],0);self.assertEqual(self.c.execute("SELECT count(*) FROM memory_fts WHERE memory_fts MATCH 'Python'").fetchone()[0],1);self.assertEqual(self.c.execute('SELECT count(*) FROM memory_revisions').fetchone()[0],2)
    def test_short_terms_are_indexed(self):
        proposal(self.c,value='SQL API MCP SSE');resolve(self.c,'p1');self.assertEqual(len(self.c.execute(RECALL,('"sql" OR "api"','global')).fetchall()),1)
    def test_scope_and_global_override(self):
        proposal(self.c,'g',value='SQL');resolve(self.c,'g');proposal(self.c,'a',scope='a',value='Rust');resolve(self.c,'a','a');proposal(self.c,'b',scope='b',value='Python');resolve(self.c,'b','b')
        rows=self.c.execute(RECALL,('"sql" OR "rust" OR "python"','a')).fetchall();self.assertEqual([r[3] for r in rows],['Rust'])
    def test_candidate_dedup_constraint(self):
        proposal(self.c)
        with self.assertRaises(sqlite3.IntegrityError):proposal(self.c,'p2')
    def test_job_key_is_idempotent(self):
        query="INSERT INTO jobs(id,job_key,scope,source_id,payload,status,available_at,created_at) VALUES(?,?,'global','source','[]','pending',0,'now')"
        self.c.execute(query,('j1','once'))
        with self.assertRaises(sqlite3.IntegrityError):self.c.execute(query,('j2','once'))
    def test_foreign_key_requires_candidate(self):
        with self.assertRaises(sqlite3.IntegrityError):self.c.execute(UPSERT,('m','global','language','Rust','preference',1,'missing','now'))
    def test_online_backup_reads_wal_and_refuses_overwrite(self):
        with tempfile.TemporaryDirectory() as d:
            src=Path(d)/'source.db';dst=Path(d)/'backup.db';c=sqlite3.connect(src);c.execute('PRAGMA journal_mode=WAL');c.execute('CREATE TABLE example(value)');c.execute('INSERT INTO example VALUES(42)');c.commit()
            backup(src,dst)
            with sqlite3.connect(dst) as out:self.assertEqual(out.execute('SELECT value FROM example').fetchone()[0],42)
            with self.assertRaises(FileExistsError):backup(src,dst)
            c.close()
    def test_legacy_migration_is_quarantined_and_filters_secrets(self):
        with tempfile.TemporaryDirectory() as d:
            src=Path(d)/'old.db';dst=Path(d)/'new.db'
            with sqlite3.connect(src) as old:
                old.execute('CREATE TABLE memories(key,value,category,status)');old.executemany('INSERT INTO memories VALUES(?,?,?,?)',[('language','Rust','preference','active'),('api_key','synthetic','credential','active'),('project','old','fact','archived')])
            result=migrate(src,dst)
            self.assertEqual(result,{'candidates_imported':1,'sensitive_skipped':1,'invalid_skipped':0,'inactive_skipped':1})
            with sqlite3.connect(dst) as new:
                self.assertEqual(new.execute('SELECT count(*) FROM memories').fetchone()[0],0);self.assertEqual(new.execute('SELECT scope,status FROM candidates').fetchone(),('legacy-review','pending'))
            with self.assertRaises(ValueError):migrate(src,dst)
            with sqlite3.connect(src) as old:self.assertEqual(old.execute('SELECT count(*) FROM memories').fetchone()[0],3)

if __name__=='__main__':unittest.main(verbosity=2)
