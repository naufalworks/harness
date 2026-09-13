import hashlib
import os
import sqlite3
import sys
import tempfile
import unittest
from contextlib import closing
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from backup import create_encrypted_backup, generate_key, restore_encrypted_backup, restore_drill


class EncryptedBackupTests(unittest.TestCase):
    def fixture(self, directory):
        source = Path(directory) / "source.db"
        connection = sqlite3.connect(source)
        connection.execute("PRAGMA journal_mode=WAL")
        connection.execute("PRAGMA user_version=6")
        connection.execute("CREATE TABLE example(value TEXT)")
        connection.execute("INSERT INTO example VALUES('committed WAL value')")
        connection.commit()
        self.addCleanup(connection.close)
        key = generate_key(Path(directory) / "backup.key")
        return source, key

    @staticmethod
    def digest(path):
        return hashlib.sha256(Path(path).read_bytes()).hexdigest()

    def test_rotating_encrypted_backups_restore_on_a_clean_target(self):
        with tempfile.TemporaryDirectory() as directory:
            source, key = self.fixture(directory)
            backup_dir = Path(directory) / "backups"
            archives = []
            for second in range(3):
                archives.append(create_encrypted_backup(
                    source, backup_dir, key, retain=2,
                    clock=datetime(2026, 9, 13, 1, 0, second, tzinfo=timezone.utc),
                ))
            retained = sorted(backup_dir.glob("harness-*.hbak"))
            self.assertEqual(len(retained), 2)
            self.assertFalse(archives[0].exists())
            self.assertNotEqual(retained[-1].read_bytes()[:16], b"SQLite format 3\x00")
            self.assertEqual(retained[-1].stat().st_mode & 0o777, 0o600)
            self.assertTrue(restore_drill(retained[-1], key))
            target = Path(directory) / "clean" / "restored.db"
            restore_encrypted_backup(retained[-1], target, key)
            with closing(sqlite3.connect(target)) as restored:
                self.assertEqual(restored.execute("SELECT value FROM example").fetchone()[0], "committed WAL value")
                self.assertEqual(restored.execute("PRAGMA user_version").fetchone()[0], 6)

    def test_missing_wrong_key_and_corruption_fail_without_output_or_source_change(self):
        with tempfile.TemporaryDirectory() as directory:
            source, key = self.fixture(directory)
            source_hash = self.digest(source)
            archive = create_encrypted_backup(source, Path(directory) / "backups", key)
            missing = Path(directory) / "missing.key"
            with self.assertRaisesRegex(ValueError, "key file not found"):
                restore_encrypted_backup(archive, Path(directory) / "missing.db", missing)
            wrong = generate_key(Path(directory) / "wrong.key")
            with self.assertRaisesRegex(ValueError, "authentication failed"):
                restore_encrypted_backup(archive, Path(directory) / "wrong.db", wrong)
            corrupt = Path(directory) / "corrupt.hbak"
            payload = bytearray(archive.read_bytes())
            payload[-1] ^= 1
            corrupt.write_bytes(payload)
            with self.assertRaisesRegex(ValueError, "authentication failed"):
                restore_encrypted_backup(corrupt, Path(directory) / "corrupt.db", key)
            self.assertFalse((Path(directory) / "missing.db").exists())
            self.assertFalse((Path(directory) / "wrong.db").exists())
            self.assertFalse((Path(directory) / "corrupt.db").exists())
            self.assertEqual(self.digest(source), source_hash)

    def test_quota_and_interrupted_writes_publish_nothing_and_preserve_source(self):
        with tempfile.TemporaryDirectory() as directory:
            source, key = self.fixture(directory)
            source_hash = self.digest(source)
            backup_dir = Path(directory) / "backups"
            with self.assertRaisesRegex(OSError, "quota exhausted"):
                create_encrypted_backup(source, backup_dir, key, max_output_bytes=32)
            with self.assertRaisesRegex(InterruptedError, "interrupted backup write"):
                create_encrypted_backup(source, backup_dir, key, interrupt_after_bytes=64)
            self.assertEqual(list(backup_dir.glob("*")), [])
            self.assertEqual(self.digest(source), source_hash)

    def test_key_file_permissions_are_enforced(self):
        if os.name != "posix":
            self.skipTest("POSIX mode check")
        with tempfile.TemporaryDirectory() as directory:
            source, key = self.fixture(directory)
            key.chmod(0o644)
            with self.assertRaisesRegex(PermissionError, "owner-only"):
                create_encrypted_backup(source, Path(directory) / "backups", key)


if __name__ == "__main__":
    unittest.main()
