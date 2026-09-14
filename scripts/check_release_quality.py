#!/usr/bin/env python3
"""Fail-closed local evidence for P12-T06; CI-only evidence is reported, never passed."""
from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT=Path(__file__).resolve().parents[1]

PROPERTIES={
 'redaction':'stream_redactor_equals_redact_for_every_chunking',
 'sse':'activity_stream_frames_recorded_rows_and_resumes_from_the_cursor',
 'diff':'reverse_rebuilds_the_original_from_recorded_changes',
 'path':'read_numbers_lines_reports_totals_and_honours_the_sandbox',
 'graph':'bounded_incident_graph_never_returns_dangling_references',
 'context':'context_budgets_every_category_and_receipts_exclusions',
}
CI_REQUIRED=(
 'cargo llvm-cov', 'aarch64-unknown-linux-gnu', 'cmp target-a/release/harness',
 'sbom.cdx.json', 'cosign', 'public_smoke.py', 'HARNESS_PUBLIC_SMOKE_TOKEN',
)

def run(name,*command,env=None):
 print(f'[RUN] {name}')
 subprocess.run(command,cwd=ROOT,check=True,env=env)
 print(f'[PASS] {name}')

def main():
 workflow=(ROOT/'.github/workflows/ci.yml').read_text()
 absent=[token for token in CI_REQUIRED if token not in workflow]
 if absent: raise SystemExit(f'release-evidence CI contract shrank: {absent}')
 listing=subprocess.run(['cargo','test','--locked','--','--list'],cwd=ROOT,text=True,capture_output=True,check=True).stdout
 missing={kind:test for kind,test in PROPERTIES.items() if not re.search(rf'(^|::){re.escape(test)}:',listing,re.MULTILINE)}
 if missing: raise SystemExit(f'property inventory shrank: {missing}')
 for kind,test in PROPERTIES.items(): run('property-'+kind,'cargo','test','--locked',test)
 run('import-protocol-fuzz','python3','tests/fuzz_import_protocol.py')
 run('performance-budget','python3','scripts/benchmark.py','--check','--sessions','10000','--events','100000')
 run('rollback','python3','tests/deploy_rollback.py')
 with tempfile.TemporaryDirectory(prefix='harness-release-evidence-') as td:
  first=Path(td)/'first'; second=Path(td)/'second'
  # Build into a dedicated target directory: overwriting target/release/harness would
  # desynchronise the on-disk executable from the running service and block deploy.sh.
  build_dir=str(ROOT/'target-release-evidence')
  env={**os.environ,'HARNESS_RELEASE_DIR':str(first),'CARGO_TARGET_DIR':build_dir}
  run('release-artifact','bash','scripts/release.sh',env=env)
  archives=list(first.glob('*.tar.gz'))
  if len(archives)!=1 or not Path(str(archives[0])+'.sha256').is_file(): raise SystemExit('release artifact/checksum missing')
  subprocess.run(['sha256sum','-c',archives[0].name+'.sha256'],cwd=first,check=True)
  names=subprocess.run(['tar','-tzf',str(archives[0])],text=True,capture_output=True,check=True).stdout
  for suffix in ('/harness','/sbom.cdx.json','/MANIFEST.sha256','/release.json'):
   if suffix not in names: raise SystemExit(f'artifact missing {suffix}')
  env={**os.environ,'HARNESS_RELEASE_DIR':str(second),'CARGO_TARGET_DIR':build_dir,'HARNESS_RELEASE_SKIP_BUILD':'1'}
  run('release-reproducibility','bash','scripts/release.sh',env=env)
  second_archive=list(second.glob('*.tar.gz'))
  if len(second_archive)!=1 or archives[0].read_bytes()!=second_archive[0].read_bytes():
   raise SystemExit('release archive is not byte-for-byte reproducible')
 print('[PASS] artifact SBOM/checksum/layout/reproducibility')
 for tool,claim in [('cargo-llvm-cov','coverage'),('cosign','signature')]:
  state='available but CI owns the authoritative run' if shutil.which(tool) else 'not installed locally'
  print(f'[SKIP] {claim}: {state}; required by release-evidence CI')
 print('[SKIP] cross-target: only the host target is installed; x86_64 and aarch64 checks are required by release-evidence CI')
 if not os.environ.get('HARNESS_PUBLIC_URL'):
  print('[SKIP] public-smoke: HARNESS_PUBLIC_URL is unset; required by deployment workflow')
 print('[PASS] P12-T06 local runnable evidence complete; inspect [SKIP] lines')

if __name__=='__main__':
 try: main()
 except subprocess.CalledProcessError as error: sys.exit(error.returncode or 1)
