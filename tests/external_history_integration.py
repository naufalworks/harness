#!/usr/bin/env python3
"""P19-T03 real-server ingestion, restart, lost-ack and failure tests. No MCP client.
Builds the current binary sequentially; uses only a disposable DB and loopback server.
"""
import copy
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
import time
import unittest
import urllib.request
import urllib.error
from contextlib import closing
from concurrent.futures import ThreadPoolExecutor
from test_external_history_contract import digest
ROOT = Path(__file__).resolve().parents[1]
TOKEN = 'p' * 40
OWNER = 'o' * 40

class ExternalHistory(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        env = dict(os.environ, CARGO_BUILD_JOBS='1', RUST_TEST_THREADS='1')
        subprocess.run(['cargo', 'build', '--locked'], cwd=ROOT, env=env, check=True)

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='harness-external-history-')
        self.root = Path(self.tmp.name)
        self.db = self.root / 'history.db'
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            self.port = sock.getsockname()[1]
        self.base = f'http://127.0.0.1:{self.port}'
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('HARNESS_')}
        self.env.update(HARNESS_DB=str(self.db), HARNESS_ADDR=f'127.0.0.1:{self.port}',
                        HARNESS_AUTH_TOKEN=OWNER, HARNESS_API_KEY='synthetic',
                        HARNESS_BASE_URL='http://127.0.0.1:9', HARNESS_MODEL='synthetic',
                        HARNESS_HISTORY_PRODUCERS=json.dumps([
                            dict(producer_id='development-mcp', token=TOKEN, projects=['proj-harness', 'other']),
                            dict(producer_id='second', token='q'*40, projects=['proj-harness'])]))
        self.app = None
        self.start()

    def start(self):
        with (self.root / 'server.log').open('ab') as log:
            self.app = subprocess.Popen([str(ROOT/'target/debug/harness')], cwd=self.root,
                                        env=self.env, stdout=subprocess.DEVNULL, stderr=log)
        for _ in range(100):
            if self.app.poll() is not None:
                self.fail('disposable server exited on startup')
            try:
                if self.call('/memory/status', token=OWNER, method='GET')[0] == 200:
                    return
            except (OSError, urllib.error.URLError):
                pass
            time.sleep(.05)
        self.fail('disposable server startup timeout')

    def stop(self):
        if self.app and self.app.poll() is None:
            self.app.kill()
            self.app.wait(timeout=10)

    def tearDown(self):
        self.stop()
        self.tmp.cleanup()

    def call(self, path='/external-history/events', body=None, token=TOKEN, method='POST'):
        if body is None and method == 'POST':
            body = self.event()
        data = body if isinstance(body, bytes) else (json.dumps(body).encode() if body is not None else None)
        request = urllib.request.Request(self.base+path, data=data, method=method,
                    headers={'Authorization': 'Bearer '+token, 'Content-Type':'application/json'})
        try:
            with urllib.request.urlopen(request, timeout=15) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as exc:
            with exc:
                return exc.code, json.load(exc)

    def event(self, **changes):
        e = json.loads((ROOT/'tests/external_history/accepted/tool_completed.json').read_text())
        e.update(changes)
        e['content_digest'] = digest(e)
        return e

    def count(self):
        with closing(sqlite3.connect(self.db)) as db:
            return db.execute('SELECT count(*) FROM external_history_events').fetchone()[0]

    def test_restart_replay_conflict_and_no_execution(self):
        e = self.event()
        code, first = self.call(body=e)
        self.assertEqual(code,201)
        self.assertEqual(first['state'],'committed')
        self.assertEqual(self.count(),1)
        self.stop(); self.start()
        code, again = self.call(body=e)
        self.assertEqual(code,200)
        for key in ['receipt_id','ingested_at']:
            self.assertEqual(first[key],again[key])
        changed = self.event(project_id='other')
        self.assertEqual(self.call(body=changed)[0],409)
        self.assertEqual(self.count(),1)
        with closing(sqlite3.connect(self.db)) as db:
            for table in ['chat_receipts','turn_steps','provider_calls','recording_outbox','external_effects','jobs']:
                self.assertEqual(db.execute(f'SELECT count(*) FROM {table}').fetchone()[0],0,table)

    def test_lost_acknowledgement_retry(self):
        e = self.event(event_id='lost-ack')
        body=json.dumps(e).encode()
        with socket.create_connection(('127.0.0.1',self.port)) as sock:
            headers=(f'POST /external-history/events HTTP/1.1\r\nHost: 127.0.0.1:{self.port}\r\n'
                     f'Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {len(body)}\r\n\r\n').encode()
            sock.sendall(headers+body)
            # Confirm durable commit while deliberately never reading the response.
            for _ in range(100):
                if self.count()==1: break
                time.sleep(.02)
            self.assertEqual(self.count(),1)
        self.stop(); self.start()
        code, receipt=self.call(body=e)
        self.assertEqual(code,200)
        self.assertTrue(receipt['replay'])
        self.assertEqual(self.count(),1)

    def test_concurrent_replays_late_events_and_producer_isolation(self):
        with ThreadPoolExecutor(max_workers=4) as pool:
            results=list(pool.map(lambda _: self.call(),range(4)))
        self.assertEqual(sorted(c for c,_ in results),[200,200,200,201])
        self.assertEqual(len({r['receipt_id'] for _,r in results}),1)
        self.assertEqual(self.call(body=self.event(event_id='late',producer_sequence=1))[0],201)
        self.assertEqual(self.call(body=self.event(producer_id='second'),token='q'*40)[0],201)
        self.assertEqual(self.count(),3)

    def test_rejections_and_failed_write_are_not_acknowledged(self):
        for token in [OWNER,'unknown']:
            self.assertEqual(self.call(token=token)[0],401)
        self.assertEqual(self.call(body=self.event(project_id='forbidden'))[0],403)
        e=self.event(); e['payload']['output']='Bearer synthetic-canary'
        self.assertEqual(self.call(body=e)[0],400)
        e=self.event(); e['payload']['result']={'changed':True}
        self.assertEqual(self.call(body=e)[1]['code'],'invalid_digest')
        self.assertEqual(self.call(body=b'x'*65537)[0],413)
        self.assertEqual(self.call(body=b'{"a":1,"a":2}')[0],400)
        for path in ['/history/search','/archives/a']:
            self.assertEqual(self.call(path,method='GET')[0],401)
        self.assertEqual(self.count(),0)
        with closing(sqlite3.connect(self.db)) as db:
            db.execute("CREATE TRIGGER reject_external BEFORE INSERT ON external_history_events BEGIN SELECT RAISE(ABORT,'synthetic-canary'); END;")
            db.commit()
        code, result=self.call()
        self.assertEqual(code,503)
        self.assertNotIn('synthetic-canary',json.dumps(result))
        self.assertEqual(self.count(),0)
        with closing(sqlite3.connect(self.db)) as db:
            db.execute('DROP TRIGGER reject_external'); db.commit()
        self.assertEqual(self.call()[0],201)
        self.assertNotIn('synthetic-canary',(self.root/'server.log').read_text())

    def test_fixture_corpus_and_append_only(self):
        for path in sorted((ROOT/'tests/external_history/accepted').glob('*.json')):
            e=json.loads(path.read_text())
            code,_=self.call(body=e)
            self.assertIn(code,[200,201],path.name)
        before=self.count()
        for path in sorted((ROOT/'tests/external_history/rejected').glob('*.json')):
            e=json.loads(path.read_text()); e.pop('__expect')
            code,_=self.call(body=e)
            self.assertIn(code,[400,403,409],path.name)
        self.assertEqual(self.count(),before)
        with closing(sqlite3.connect(self.db)) as db:
            for sql in ['UPDATE external_history_events SET event_id=event_id','DELETE FROM external_history_events']:
                with self.assertRaises(sqlite3.DatabaseError): db.execute(sql)
                db.rollback()

if __name__=='__main__':
    unittest.main(verbosity=2)
