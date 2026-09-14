#!/usr/bin/env python3
"""Bounded deterministic fuzz smoke for import boundaries and protocol parsers."""
from __future__ import annotations
import importlib.util, json, random, subprocess, sys, tempfile
from pathlib import Path

ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT/'scripts'))

def load(name,path):
    spec=importlib.util.spec_from_file_location(name,path)
    module=importlib.util.module_from_spec(spec)
    assert spec and spec.loader
    spec.loader.exec_module(module)
    return module

imp=load('import_sessions',ROOT/'scripts/import_sessions.py')
legacy=load('migrate_legacy',ROOT/'scripts/migrate_legacy.py')
trusted=load('migrate_legacy_trusted',ROOT/'scripts/migrate_legacy_trusted.py')


def main():
    rng=random.Random(0x12_06)
    alphabet='abcXYZ09_ .:/-\n\t😀é'
    for _ in range(2000):
        text=''.join(rng.choice(alphabet) for _ in range(rng.randrange(0,300)))
        assert isinstance(legacy.sensitive(text),bool)
        assert isinstance(trusted.sensitive(text),bool)
        if any(marker in text.lower() for marker in legacy.MARKERS):
            assert legacy.sensitive(text) and trusted.sensitive(text)
        json.loads(json.dumps({'name':'fuzz.jsonl','scope':'global','content':text},ensure_ascii=False))
    with tempfile.TemporaryDirectory(prefix='harness-import-fuzz-') as td:
        root=Path(td)
        expected=[]
        for index in range(75):
            path=root/(f'{index:03}.jsonl' if index%3 else f'ignored-{index}.txt')
            path.write_bytes(bytes(rng.randrange(0,256) for _ in range(rng.randrange(0,200))))
            if path.suffix=='.jsonl': expected.append(path)
        found=imp.files_at(root,100)
        assert found==sorted(expected)
        try: imp.files_at(root,len(expected)-1)
        except ValueError as error: assert 'File-count limit' in str(error)
        else: raise AssertionError('file-count bound did not fail closed')
        link=root/'escape.jsonl'; link.symlink_to('/etc/passwd')
        assert link not in imp.files_at(root,100)
    # Exercise the production provider protocol parser's exhaustive byte-boundary and invalid-input tests.
    subprocess.run(['cargo','test','--locked','streaming_'],cwd=ROOT,check=True)
    print('bounded fuzz PASS: 2000 import payloads, file/symlink/count bounds, protocol byte boundaries')

if __name__=='__main__': main()
