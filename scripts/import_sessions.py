#!/usr/bin/env python3
"""Upload bounded local transcripts; the service never accepts filesystem paths."""
import argparse,json,os,time,urllib.request,urllib.error,urllib.parse
from pathlib import Path

def files_at(path,max_files):
    if path.is_symlink():raise ValueError('Symlinks are not accepted')
    if path.is_file():return [path]
    if not path.is_dir():raise ValueError('Path not found')
    result=[];root=path.resolve()
    for directory,dirs,files in os.walk(root,followlinks=False):
        dirs[:]=sorted(d for d in dirs if not (Path(directory)/d).is_symlink() and len((Path(directory)/d).relative_to(root).parts)<=12)
        for name in sorted(files):
            f=Path(directory)/name
            if not f.is_symlink() and (f.suffix=='.jsonl' or name=='transcript.md'):
                result.append(f)
                if len(result)>max_files:raise ValueError('File-count limit exceeded; narrow the input directory')
    return result

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('path');p.add_argument('--scope',default='global');p.add_argument('--format',choices=['omp','claude','codex','junie']);p.add_argument('--url',default='http://127.0.0.1:8080');p.add_argument('--max-files',type=int,default=200);p.add_argument('--send-to-provider',action='store_true',help='Acknowledge that sanitized messages are queued for provider extraction');a=p.parse_args()
    if not a.send_to_provider:p.error('--send-to-provider is required; inspect source files for secrets first')
    token=os.environ.get('HARNESS_AUTH_TOKEN','')
    if not token:p.error('HARNESS_AUTH_TOKEN is required in the environment')
    target=urllib.parse.urlsplit(a.url)
    if target.scheme!='http' or target.hostname not in {'127.0.0.1','localhost','::1'} or target.username or target.password or target.query or target.fragment or target.path not in {'','/'}:p.error('Only loopback service URLs without userinfo/path are supported')
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self,*args,**kwargs):return None
    opener=urllib.request.build_opener(NoRedirect)
    for f in files_at(Path(a.path),a.max_files):
        try:
            if f.stat().st_size>1048576:raise ValueError('File exceeds 1 MiB; split into valid transcript files first')
            body={'name':f.name,'scope':a.scope,'content':f.read_text(encoding='utf-8')}
            if a.format:body['format']=a.format
            req=urllib.request.Request(a.url.rstrip('/')+'/memory/ingest',json.dumps(body).encode(),headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'},method='POST')
            with opener.open(req,timeout=30) as response:result=json.load(response)
            print(json.dumps({'file':f.name,**result}))
        except (ValueError,OSError,UnicodeError,urllib.error.URLError):
            print(json.dumps({'file':f.name,'error':'Import failed; check size, format, service status and scope'}))
        time.sleep(.1)
if __name__=='__main__':main()
