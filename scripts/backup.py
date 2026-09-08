#!/usr/bin/env python3
"""Consistent SQLite online backup, including committed WAL state. Never overwrites a target."""
import argparse, os, sqlite3
from pathlib import Path

def backup(source, destination):
    source=Path(source).resolve(); destination=Path(destination).resolve()
    if not source.is_file(): raise ValueError('Source database not found')
    if source==destination: raise ValueError('Backup target must differ from source')
    destination.parent.mkdir(parents=True,exist_ok=True)
    fd=os.open(destination,os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600);os.close(fd)
    try:
        with sqlite3.connect(source.as_uri()+'?mode=ro',uri=True) as src, sqlite3.connect(destination) as dst:
            src.backup(dst,pages=256,sleep=0.05)
            result=dst.execute('PRAGMA integrity_check').fetchone()[0]
            if result!='ok':raise RuntimeError('Backup integrity check failed')
    except Exception:
        destination.unlink(missing_ok=True);raise
    return destination

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('source');p.add_argument('destination');a=p.parse_args()
    print('Backup verified:',backup(a.source,a.destination))
