#!/usr/bin/env python3
"""Read-only smoke test for an explicitly configured deployed HTTPS endpoint."""
from __future__ import annotations
import argparse, json, os, ssl, urllib.parse, urllib.request


def main() -> int:
    parser=argparse.ArgumentParser()
    parser.add_argument('--url', required=True)
    parser.add_argument('--token', default=os.environ.get('HARNESS_PUBLIC_SMOKE_TOKEN'))
    args=parser.parse_args()
    if not args.token:
        parser.error('--token or HARNESS_PUBLIC_SMOKE_TOKEN is required')
    parsed=urllib.parse.urlsplit(args.url)
    if (parsed.scheme != 'https' or parsed.username or parsed.password or parsed.query
            or parsed.fragment or parsed.path not in {'', '/'}):
        parser.error('--url must be a credential-free HTTPS origin')
    origin=f"{parsed.scheme}://{parsed.netloc}"
    request=urllib.request.Request(origin+'/health',headers={'Authorization':'Bearer '+args.token})
    with urllib.request.urlopen(request,timeout=20,context=ssl.create_default_context()) as response:
        payload=json.load(response)
    required={'ready','commit','binary_sha256','schema_version','database','workers'}
    missing=required-payload.keys()
    if missing or payload.get('ready') is not True or payload.get('database',{}).get('ready') is not True:
        raise SystemExit(f'public smoke failed: missing={sorted(missing)} ready={payload.get("ready")}')
    print('public smoke PASS: authenticated HTTPS health is ready and identifies build/schema')
    return 0

if __name__ == '__main__':
    raise SystemExit(main())
