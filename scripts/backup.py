#!/usr/bin/env python3
"""Create and restore verified, rotating encrypted SQLite backups.

The encryption key is supplied by a separate owner-only file. Archive publication is
atomic: a verified archive replaces its temporary file only after fsync succeeds.
"""
import argparse
import base64
import errno
import hashlib
import json
import os
import re
import secrets
import sqlite3
import struct
import tempfile
from contextlib import closing
from datetime import datetime, timezone
from pathlib import Path

try:
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
except ImportError:  # pragma: no cover - exercised on hosts without the optional package
    AESGCM = None

MAGIC = b"HARNESS-BACKUP\x00"
VERSION = 1
ALGORITHM = "AES-256-GCM"
MAX_HEADER_BYTES = 64 * 1024
ARCHIVE_GLOB = "harness-*.hbak"


def _require_crypto():
    if AESGCM is None:
        raise RuntimeError("encrypted backup requires the Python 'cryptography' package")


def _read_key(key_file):
    path = Path(key_file).resolve()
    if not path.is_file():
        raise ValueError("Backup key file not found")
    if os.name == "posix" and path.stat().st_mode & 0o077:
        raise PermissionError("Backup key file must be owner-only (chmod 600)")
    data = path.read_bytes()
    text = data.strip()
    if re.fullmatch(rb"[0-9a-fA-F]{64}", text):
        key = bytes.fromhex(text.decode("ascii"))
    elif len(data) == 32:
        key = data
    else:
        raise ValueError("Backup key must be 32 raw bytes or 64 hexadecimal characters")
    return key


def generate_key(key_file):
    path = Path(key_file).resolve()
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    try:
        with os.fdopen(fd, "wb", closefd=True) as stream:
            stream.write(secrets.token_hex(32).encode("ascii") + b"\n")
            stream.flush()
            os.fsync(stream.fileno())
    except Exception:
        path.unlink(missing_ok=True)
        raise
    return path


def backup(source, destination):
    """Create a verified plaintext online snapshot for internal migration use."""
    source = Path(source).resolve()
    destination = Path(destination).resolve()
    if not source.is_file():
        raise ValueError("Source database not found")
    if source == destination:
        raise ValueError("Backup target must differ from source")
    destination.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(destination, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    os.close(fd)
    try:
        uri = source.as_uri() + "?mode=ro"
        with closing(sqlite3.connect(uri, uri=True)) as src, closing(sqlite3.connect(destination)) as dst:
            src.backup(dst, pages=256, sleep=0.05)
            if dst.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
                raise RuntimeError("Backup integrity check failed")
    except Exception:
        destination.unlink(missing_ok=True)
        raise
    return destination


def _schema_version(database):
    with closing(sqlite3.connect(f"file:{database}?mode=ro", uri=True)) as connection:
        return int(connection.execute("PRAGMA user_version").fetchone()[0])


def _encode_archive(snapshot, key, created_at):
    _require_crypto()
    plaintext = Path(snapshot).read_bytes()
    nonce = secrets.token_bytes(12)
    header = {
        "algorithm": ALGORITHM,
        "created_at": created_at,
        "key_id": hashlib.sha256(key).hexdigest()[:16],
        "nonce": base64.b64encode(nonce).decode("ascii"),
        "plaintext_sha256": hashlib.sha256(plaintext).hexdigest(),
        "schema_version": _schema_version(snapshot),
        "version": VERSION,
    }
    encoded_header = json.dumps(header, sort_keys=True, separators=(",", ":")).encode("utf-8")
    prefix = MAGIC + struct.pack(">I", len(encoded_header)) + encoded_header
    return prefix + AESGCM(key).encrypt(nonce, plaintext, prefix)


def _decode_archive(archive, key):
    _require_crypto()
    payload = Path(archive).read_bytes()
    prefix_length = len(MAGIC) + 4
    if len(payload) < prefix_length or not payload.startswith(MAGIC):
        raise ValueError("Not a Harness encrypted backup")
    header_length = struct.unpack(">I", payload[len(MAGIC):prefix_length])[0]
    if header_length < 2 or header_length > MAX_HEADER_BYTES:
        raise ValueError("Invalid encrypted backup header")
    header_end = prefix_length + header_length
    if header_end >= len(payload):
        raise ValueError("Truncated encrypted backup")
    try:
        header = json.loads(payload[prefix_length:header_end])
        nonce = base64.b64decode(header["nonce"], validate=True)
    except (KeyError, ValueError, TypeError, json.JSONDecodeError) as error:
        raise ValueError("Invalid encrypted backup header") from error
    if header.get("version") != VERSION or header.get("algorithm") != ALGORITHM or len(nonce) != 12:
        raise ValueError("Unsupported encrypted backup format")
    prefix = payload[:header_end]
    try:
        plaintext = AESGCM(key).decrypt(nonce, payload[header_end:], prefix)
    except Exception as error:
        raise ValueError("Encrypted backup authentication failed") from error
    if hashlib.sha256(plaintext).hexdigest() != header.get("plaintext_sha256"):
        raise ValueError("Encrypted backup checksum failed")
    return header, plaintext


def _atomic_publish(path, data, max_output_bytes=None, interrupt_after_bytes=None):
    path = Path(path).resolve()
    if path.exists():
        raise FileExistsError(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.parent / f".{path.name}.{secrets.token_hex(6)}.partial"
    fd = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    try:
        if max_output_bytes is not None and len(data) > max_output_bytes:
            raise OSError(errno.ENOSPC, "simulated backup quota exhausted")
        with os.fdopen(fd, "wb", closefd=True) as stream:
            if interrupt_after_bytes is not None:
                stream.write(data[:interrupt_after_bytes])
                stream.flush()
                raise InterruptedError("simulated interrupted backup write")
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory_fd = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    except Exception:
        try:
            os.close(fd)
        except OSError:
            pass
        temporary.unlink(missing_ok=True)
        path.unlink(missing_ok=True)
        raise
    return path


def restore_encrypted_backup(archive, destination, key_file):
    archive = Path(archive).resolve()
    destination = Path(destination).resolve()
    if not archive.is_file():
        raise ValueError("Encrypted backup not found")
    if destination.exists():
        raise FileExistsError(destination)
    key = _read_key(key_file)
    header, plaintext = _decode_archive(archive, key)
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.parent / f".{destination.name}.{secrets.token_hex(6)}.partial"
    try:
        _atomic_publish(temporary, plaintext)
        with closing(sqlite3.connect(f"file:{temporary}?mode=ro", uri=True)) as connection:
            if connection.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
                raise RuntimeError("Restored database integrity check failed")
            if int(connection.execute("PRAGMA user_version").fetchone()[0]) != header["schema_version"]:
                raise RuntimeError("Restored database schema version differs from archive")
        os.replace(temporary, destination)
        directory_fd = os.open(destination.parent, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    except Exception:
        temporary.unlink(missing_ok=True)
        destination.unlink(missing_ok=True)
        raise
    return destination


def restore_drill(archive, key_file):
    with tempfile.TemporaryDirectory(prefix="harness-restore-drill-") as directory:
        restored = restore_encrypted_backup(archive, Path(directory) / "restored.db", key_file)
        with closing(sqlite3.connect(f"file:{restored}?mode=ro", uri=True)) as connection:
            return connection.execute("PRAGMA integrity_check").fetchone()[0] == "ok"


def create_encrypted_backup(source, directory, key_file, retain=7, *, max_output_bytes=None, interrupt_after_bytes=None, clock=None):
    if retain < 1:
        raise ValueError("retain must be at least 1")
    source = Path(source).resolve()
    directory = Path(directory).resolve()
    key = _read_key(key_file)
    moment = clock or datetime.now(timezone.utc)
    stamp = moment.strftime("%Y%m%dT%H%M%SZ")
    created_at = moment.isoformat()
    name = f"harness-{stamp}-{secrets.token_hex(4)}.hbak"
    archive = directory / name
    with tempfile.TemporaryDirectory(prefix="harness-backup-snapshot-") as temporary:
        snapshot = backup(source, Path(temporary) / "snapshot.db")
        encoded = _encode_archive(snapshot, key, created_at)
        _atomic_publish(archive, encoded, max_output_bytes, interrupt_after_bytes)
    try:
        if not restore_drill(archive, key_file):
            raise RuntimeError("Clean restore drill failed")
    except Exception:
        archive.unlink(missing_ok=True)
        raise
    archives = sorted(directory.glob(ARCHIVE_GLOB), key=lambda path: (path.stat().st_mtime_ns, path.name))
    for expired in archives[:-retain]:
        expired.unlink()
    return archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    keygen = commands.add_parser("keygen", help="create a new owner-only 256-bit key file")
    keygen.add_argument("key_file")
    create = commands.add_parser("create", help="create, verify, drill and rotate encrypted backups")
    create.add_argument("source")
    create.add_argument("directory")
    create.add_argument("--key-file", required=True)
    create.add_argument("--retain", type=int, default=7)
    restore = commands.add_parser("restore", help="restore one archive into a new database path")
    restore.add_argument("archive")
    restore.add_argument("destination")
    restore.add_argument("--key-file", required=True)
    drill = commands.add_parser("drill", help="restore into a clean temporary target and verify")
    drill.add_argument("archive")
    drill.add_argument("--key-file", required=True)
    args = parser.parse_args()
    if args.command == "keygen":
        print("Key created:", generate_key(args.key_file))
    elif args.command == "create":
        print("Encrypted backup verified:", create_encrypted_backup(args.source, args.directory, args.key_file, args.retain))
    elif args.command == "restore":
        print("Backup restored and verified:", restore_encrypted_backup(args.archive, args.destination, args.key_file))
    else:
        if not restore_drill(args.archive, args.key_file):
            raise SystemExit("Restore drill failed")
        print("Restore drill passed")


if __name__ == "__main__":
    main()
