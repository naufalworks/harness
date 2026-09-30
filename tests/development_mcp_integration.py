"""P19-T08: real producer tools and delivery against a disposable Harness server.

Set HARNESS_DEVELOPMENT_MCP_ROOT to the matching producer checkout. No live
configuration, state directory or production network endpoint is used.
"""
from __future__ import annotations

import json
import os
import sqlite3
import sys
import tempfile
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from pathlib import Path

PRODUCER = Path(os.environ.get('HARNESS_DEVELOPMENT_MCP_ROOT', '')).resolve()
if not (PRODUCER / 'src/notion_local_ops_mcp/exporter.py').is_file():
    raise SystemExit('BLOCKED: matching development-mcp checkout required via HARNESS_DEVELOPMENT_MCP_ROOT')

# The remote tool process exports live NOTION_LOCAL_OPS_* values. Strip them
# before importing server.py, which constructs its TaskStore on import.
for name in list(os.environ):
    if name.startswith('NOTION_LOCAL_OPS_'):
        del os.environ[name]
IMPORT_STATE = tempfile.TemporaryDirectory(prefix='p19-t08-producer-import-')
os.environ['NOTION_LOCAL_OPS_STATE_DIR'] = IMPORT_STATE.name
sys.path.insert(0, str(PRODUCER / 'src'))

import external_history_integration as harness_tests
from notion_local_ops_mcp import capture, exporter, server, session
from notion_local_ops_mcp.capture import (
    CaptureJournal,
    CaptureUnavailable,
)
from notion_local_ops_mcp.executors import ExecutorRegistry
from notion_local_ops_mcp.tasks import TaskStore


class DevelopmentMcpIntegration(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        harness_tests.ExternalHistory.setUpClass()

    def setUp(self):
        self.harness = harness_tests.ExternalHistory('test_fixture_corpus_and_append_only')
        self.harness.setUp()
        self.work = self.harness.root / 'producer-work'
        self.work.mkdir()
        self.old = (server.WORKSPACE_ROOT, server.store, server.registry,
                    capture.get_journal(), exporter.get_exporter())
        server.WORKSPACE_ROOT = self.work
        server.store = TaskStore(self.harness.root / 'producer-tasks')
        server.registry = ExecutorRegistry(store=server.store,
            codex_command='nonexistent-codex', claude_command='nonexistent-claude')
        session.reset_all_sessions()
        self.journal = CaptureJournal(self.harness.root / 'producer-capture', mode='required')
        capture.set_journal(self.journal)
        self.worker = exporter.HistoryExporter(self.journal, url=self.harness.base,
            token=harness_tests.TOKEN, producer_id='development-mcp', project_id='proj-harness')
        exporter.set_exporter(self.worker)

    def tearDown(self):
        self.worker.stop()
        self.journal.close()
        exporter.set_exporter(self.old[4])
        capture.set_journal(self.old[3])
        server.WORKSPACE_ROOT, server.store, server.registry = self.old[:3]
        session.reset_all_sessions()
        self.harness.tearDown()

    def envelopes(self):
        with closing(sqlite3.connect(self.harness.db)) as db:
            return [json.loads(row[0]) for row in db.execute(
                "SELECT envelope FROM external_history_events ORDER BY rowid")]

    def test_tools_clients_background_and_bounded_output(self):
        secret = 'synthetic-secret-do-not-report'
        self.assertTrue(server.write_file('notes.txt', 'alpha\n')['success'])
        self.assertIn('alpha', server.read_text('notes.txt')['content'])
        patch = f'*** Begin Patch\n*** Update File: {self.work / "notes.txt"}\n@@\n-alpha\n+beta\n*** End Patch'
        self.assertTrue(server.apply_patch(patch)['success'])
        self.assertEqual((self.work / 'notes.txt').read_text(), 'beta\n')
        test_source = ('import unittest\nfrom pathlib import Path\n'
                       'class Sample(unittest.TestCase):\n'
                       '    def test_file(self):\n'
                       '        self.assertEqual(Path("notes.txt").read_text(), "beta\\n")\n')
        self.assertTrue(server.write_file('test_sample.py', test_source)['success'])
        result = server.run_command('python3 -m unittest -q test_sample', cwd=str(self.work))
        self.assertEqual(result['exit_code'], 0)
        self.assertIn('OK', result['stderr'])
        oversized = server.run_command(
            f"python3 -c \"print('{secret}' + 'x'*70000)\"", cwd=str(self.work))
        self.assertEqual(oversized['exit_code'], 0)
        queued = server.run_command_stream("python3 -c \"print('background done')\"",
                                           cwd=str(self.work))
        finished = server.wait_task(queued['task_id'], timeout=15)
        self.assertEqual(finished['status'], 'succeeded')

        def client(name):
            token = session.bind_session(name)
            try:
                return server.read_text('notes.txt')['success']
            finally:
                session.unbind_session(token)

        with ThreadPoolExecutor(max_workers=2) as pool:
            self.assertEqual(list(pool.map(client, ('client-a', 'client-b'))), [True, True])
        rows = self.journal.list_admissions()
        self.assertIn('client-a', {row['session_id'] for row in rows})
        self.assertIn('client-b', {row['session_id'] for row in rows})
        self.assertNotIn(secret, json.dumps(rows))
        self.assertTrue(self.worker.start())
        deadline = time.monotonic() + 20
        while self.worker.stats()['pending'] and time.monotonic() < deadline:
            time.sleep(.05)
        self.assertEqual(self.worker.stats()['pending'], 0, self.worker.stats())
        self.assertEqual(self.worker.stats()['delivered'], len(rows))
        self.assertEqual(self.worker.stats()['sequence_gaps'], 0)
        events = self.envelopes()
        self.assertEqual(len(events), len(rows))
        self.assertEqual({e['project_id'] for e in events}, {'proj-harness'})
        self.assertTrue(all(e['capture']['conversation'] == 'unavailable' for e in events))
        self.assertTrue(all(len(json.dumps(e).encode()) < 65536 for e in events))
        self.assertNotIn(secret, json.dumps(events))
        self.assertTrue(all(e['event_type'] == 'tool.completed' for e in events))

    def test_crash_outage_lost_ack_rejection_and_restore(self):
        side_effect = self.work / 'executed-once.txt'
        side_effect.write_text('one execution\n')
        incomplete = self.journal.admit(session_id='crashed-client', tool='write_file',
            title='write_file', args_summary={'bytes': 14}, cwd=None, relay_request_id=None)
        self.journal.close()
        restarted = CaptureJournal(self.journal.path.parent, mode='required')
        self.assertEqual(restarted.recover_unfinished(), [incomplete])
        capture.set_journal(restarted)
        self.journal = restarted
        self.worker.stop()
        self.worker = exporter.HistoryExporter(restarted, url=self.harness.base,
            token=harness_tests.TOKEN, producer_id='development-mcp', project_id='proj-harness')
        exporter.set_exporter(self.worker)
        self.assertEqual(self.worker.stats()['recovered_unknown'], 1)
        self.harness.stop()
        self.assertEqual(self.worker.drain(), 0)
        self.assertEqual(self.worker.stats()['pending'], 1)
        self.assertIsNotNone(self.worker.stats()['backlog_oldest_seconds'])
        self.harness.start()
        self.assertEqual(self.worker.drain(), 1)
        self.assertEqual(self.envelopes()[0]['outcome']['execution'], 'unknown')
        self.assertEqual(side_effect.read_text(), 'one execution\n')

        server.read_text('executed-once.txt')
        real_post = self.worker._post
        def lost_ack(envelope):
            status, body = real_post(envelope)
            self.assertEqual(status, 201, body)
            raise TimeoutError('lost acknowledgement')
        self.worker._post = lost_ack
        self.assertEqual(self.worker.drain(), 0)
        before = len(self.envelopes())
        self.worker._post = real_post
        self.assertEqual(self.worker.drain(), 1)
        self.assertEqual(len(self.envelopes()), before)

        server.read_text('executed-once.txt')
        self.worker.project_id = 'ungranted'
        self.assertEqual(self.worker.drain(), 0)
        self.assertEqual(self.worker.stats()['rejected'], 1)
        self.assertEqual(self.worker.stats()['last_error'], 'harness_http_403')
        self.worker.project_id = 'proj-harness'

        # SQLite online backup/restore retains immutable identities and receipts.
        self.harness.stop()
        backup = self.harness.root / 'restored-harness.db'
        with closing(sqlite3.connect(self.harness.db)) as source, closing(sqlite3.connect(backup)) as dest:
            source.backup(dest)
        self.harness.db.unlink()
        backup.rename(self.harness.db)
        self.harness.start()
        self.assertEqual(len(self.envelopes()), before)
        first = self.envelopes()[0]
        self.assertEqual(self.harness.call(body=first)[0], 200)
        self.assertEqual(len(self.envelopes()), before)
        with closing(sqlite3.connect(self.harness.db)) as db, self.assertRaises(sqlite3.IntegrityError):
            db.execute('DELETE FROM external_history_events')
        # No automatic expiry/deletion job exists: immutable evidence survives
        # restore until a future audited retention implementation is added.

    def test_capture_storage_exhaustion_refuses_side_effect(self):
        import sqlite3 as sqlite
        original = self.journal._connect
        def full():
            raise sqlite.OperationalError('synthetic-secret disk full')
        self.journal._connect = full
        try:
            with self.assertRaises(CaptureUnavailable) as error:
                server.write_file('must-not-exist.txt', 'side effect')
            self.assertNotIn('synthetic-secret', str(error.exception))
            self.assertFalse((self.work / 'must-not-exist.txt').exists())
            self.assertEqual(self.journal.status()['failures'], 1)
        finally:
            self.journal._connect = original

    def test_disposable_delivery_capacity_sample(self):
        count = 100
        started = time.monotonic()
        for number in range(count):
            admission = self.journal.admit(session_id='capacity-client', tool='read_text',
                title='read_text', args_summary={'sample': number}, cwd=None,
                relay_request_id=None)
            self.journal.commit_outcome(admission, status='ok',
                result_summary={'lines': 1}, error=None, duration_ms=1)
        capture_seconds = time.monotonic() - started
        delivered = self.worker.drain()
        total_seconds = time.monotonic() - started
        self.assertEqual(delivered, count)
        self.assertEqual(len(self.envelopes()), count)
        self.assertEqual(self.worker.stats()['pending'], 0)
        print(f'P19 disposable capacity: {count} events; capture={capture_seconds:.3f}s; '
              f'capture+HTTP delivery={total_seconds:.3f}s; '
              f'delivery_rate={count / max(total_seconds-capture_seconds, .001):.1f}/s')


if __name__ == '__main__':
    try:
        result = unittest.main(verbosity=2, exit=False)
    finally:
        IMPORT_STATE.cleanup()
    if not result.result.wasSuccessful():
        sys.exit(1)
