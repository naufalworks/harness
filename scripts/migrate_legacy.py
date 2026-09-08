#!/usr/bin/env python3
"""Create a NEW v2 DB with non-sensitive legacy memories as unapproved candidates.
Original artifacts/settings/graph remain only in the original and its backup.
No old memory is automatically trusted, and missing provenance is never invented.
"""
import argparse, hashlib, json, os, re, sqlite3, tempfile, uuid
from datetime import datetime, timezone, timedelta
from pathlib import Path
from backup import backup
ROOT=Path(__file__).resolve().parents[1]
ALLOWED={'preference','fact','project','rule','skill'}
MARKERS=['private key','private_key','age-secret-key-','password','passwd','api_key','api-key','api key','access_token','refresh_token','client_secret','credential','authorization:','bearer ','ghp_','github_pat_','xoxb-','xoxp-']
def sensitive(text):
    return any(m in text.lower() for m in MARKERS) or bool(re.search(r'\bsk-[\w-]{10,}|\bAKIA[A-Z0-9]{16}\b',text))
def migrate(source,destination,scope='legacy-review'):
    if not re.fullmatch(r'[A-Za-z0-9_.:-]{1,80}',scope):raise ValueError('Invalid scope')
    destination=Path(destination).resolve()
    if destination.exists():raise ValueError('Destination exists; refusing to overwrite')
    counts={'candidates_imported':0,'sensitive_skipped':0,'invalid_skipped':0,'inactive_skipped':0}
    with tempfile.TemporaryDirectory(prefix='harness-migration-') as tmp:
        snap=backup(source,Path(tmp)/'snapshot.db')
        src=sqlite3.connect(snap)
        try:
            tables={r[0] for r in src.execute("SELECT name FROM sqlite_master WHERE type='table'")}
            if 'memories' not in tables or 'candidates' in tables:raise ValueError('Expected legacy memory database')
            cols={r[1] for r in src.execute('PRAGMA table_info(memories)')}
            if not {'key','value','category','status'}<=cols:raise ValueError('Unexpected legacy schema')
            fingerprint=hashlib.sha256(snap.read_bytes()).hexdigest()
            destination.parent.mkdir(parents=True,exist_ok=True)
            fd=os.open(destination,os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600);os.close(fd)
            dst=sqlite3.connect(destination)
            try:
                dst.execute('PRAGMA foreign_keys=ON');dst.executescript((ROOT/'migrations/001_core.sql').read_text())
                stamp=datetime.now(timezone.utc);source_id='legacy:'+fingerprint
                with dst:
                    for key,value,category,status in src.execute('SELECT key,value,category,status FROM memories'):
                        if status!='active':counts['inactive_skipped']+=1;continue
                        if category=='credential' or sensitive(str(key)) or sensitive(str(value)):counts['sensitive_skipped']+=1;continue
                        if category not in ALLOWED or not isinstance(key,str) or not re.fullmatch(r'[a-z0-9_]{1,80}',key) or not isinstance(value,str) or not value.strip() or len(value)>1000 or len(value.encode())>4000:
                            counts['invalid_skipped']+=1;continue
                        evidence=json.dumps({'provenance':'legacy_unknown','source_snapshot_sha256':fingerprint,'quote':None})
                        dst.execute("INSERT INTO candidates(id,scope,key,value,category,source_id,evidence,expected_revision,status,created_at,expires_at) VALUES(?,?,?,?,?,?,?,0,'pending',?,?)",(str(uuid.uuid4()),scope,key,value,category,source_id,evidence,stamp.isoformat(),int((stamp+timedelta(days=30)).timestamp())))
                        counts['candidates_imported']+=1
                if dst.execute('PRAGMA integrity_check').fetchone()[0]!='ok':raise RuntimeError('Destination integrity check failed')
            except Exception:
                dst.close();destination.unlink(missing_ok=True);raise
            else:dst.close()
        finally:src.close()
    return counts
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('source');p.add_argument('destination');p.add_argument('--scope',default='legacy-review');a=p.parse_args()
    print(json.dumps(migrate(a.source,a.destination,a.scope),indent=2))
