#!/usr/bin/env python3
"""Run against the COMPILED Rust binary with a local mock provider; no paid API calls."""
import json,os,signal,socket,subprocess,tempfile,threading,time,urllib.request,urllib.error
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
requests=[]
class Provider(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_GET(self):self.reply({'data':[{'id':'synthetic-model'}]})
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers['Content-Length'])));requests.append(body)
        if body['messages'][0]['content'].startswith('Extract at most'):
            payload=json.loads(body['messages'][-1]['content']);events=payload.get('evidence_events',payload) if isinstance(payload,dict) else payload;text='[]'
            for event in events:
                if 'I prefer Rust' in event['content']:
                    text=json.dumps([{'key':'language','value':'Rust','category':'preference','evidence_id':event['id'],'quote':'I prefer Rust'}]);break
        else:text='Synthetic assistant answer'
        self.reply({'choices':[{'message':{'content':text}}]})
    def reply(self,body):
        payload=json.dumps(body).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(payload)));self.end_headers();self.wfile.write(payload)
def free_port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
def main():
    binary=ROOT/'target/debug/harness'
    if not binary.is_file():raise SystemExit('Build first: cargo build --locked')
    provider=ThreadingHTTPServer(('127.0.0.1',0),Provider);threading.Thread(target=provider.serve_forever,daemon=True).start();port=free_port();token='synthetic-local-token-'+'x'*32
    with tempfile.TemporaryDirectory() as d:
        env={**os.environ,'HARNESS_API_KEY':'synthetic','HARNESS_AUTH_TOKEN':token,'HARNESS_ADDR':f'127.0.0.1:{port}','HARNESS_BASE_URL':f'http://127.0.0.1:{provider.server_port}','HARNESS_DB':str(Path(d)/'test.db'),'HARNESS_MODEL':'synthetic-model'}
        app=subprocess.Popen([str(binary)],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        def call(path,body=None,auth=True,origin=None):
            headers={'Content-Type':'application/json'}
            if auth:headers['Authorization']='Bearer '+token
            if origin:headers['Origin']=origin
            req=urllib.request.Request(f'http://127.0.0.1:{port}'+path,None if body is None else json.dumps(body).encode(),headers=headers)
            try:
                with urllib.request.urlopen(req,timeout=10) as response:return response.status,json.load(response)
            except urllib.error.HTTPError as e:
                raw=e.read()
                try:payload=json.loads(raw)
                except json.JSONDecodeError:payload={'error':raw.decode(errors='replace')}
                return e.code,payload
        try:
            for _ in range(100):
                if app.poll() is not None:raise AssertionError('Application exited during startup')
                try:
                    if call('/memory/status')[0]==200:break
                except urllib.error.URLError:pass
                time.sleep(.1)
            else:raise AssertionError('Application did not start')
            assert call('/memory/status',auth=False)[0]==401
            assert call('/memory/status',origin='https://untrusted.invalid')[0]==403
            code,first=call('/chat',{'prompt':'I prefer Rust','scope':'project-a'});assert code==200,(code,first)
            for _ in range(100):
                inbox=call('/memory/candidates?scope=project-a')[1]['candidates']
                if inbox:break
                time.sleep(.1)
            else:raise AssertionError('No memory proposal created')
            assert call('/memory/status')[1]['active_memories']==0
            proposal=inbox[0]['id']
            assert inbox[0]['request_id']==first['request_id']
            assert call('/memory/candidates?scope=project-a&imports_only=true')[1]['candidates']==[]
            assert call('/memory/candidates?scope=project-a&imports_only=true&chat_only=true')[0]==400
            assert call(f'/memory/candidates/{proposal}/edit',{'value':'Rust systems','scope':'project-a'})[0]==200
            edited=call(f"/memory/candidates?scope=project-a&request_id={first['request_id']}&chat_only=true")[1]['candidates'][0]
            assert edited['value']=='Rust systems' and edited['evidence']['edited'] is True
            assert call('/memory/confirm',{'confirmation_id':proposal,'scope':'wrong','confirm':True})[0]==404
            assert call('/memory/confirm',{'confirmation_id':proposal,'scope':'project-a','confirm':True})[0]==200
            assert call('/memory/confirm',{'confirmation_id':proposal,'scope':'project-a','confirm':False})[0]==409
            code,second=call('/chat',{'prompt':'Use Rust and continue','session_id':first['session_id'],'scope':'project-a'});assert code==200
            assert second['recalled_context_applied'] is True
            main=[r for r in requests if not r['messages'][0]['content'].startswith('Extract at most')][-1]
            assert any(m['role']=='assistant' and m['content']=='Synthetic assistant answer' for m in main['messages'])
            assert any(m['role']=='user' and m['content']=='I prefer Rust' for m in main['messages'])
            transcript='\n'.join(json.dumps({'type':'user','message':{'role':'user','content':f'Note {i}: I prefer Rust 🦀'}}) for i in range(60))
            imported={'name':'synthetic.jsonl','format':'claude','content':transcript,'scope':'import-test'}
            assert call('/memory/ingest',imported)[0]==202
            assert call('/memory/ingest',imported)[1]['duplicate'] is True
            assert call('/memory/ingest',{'path':'/unapproved/path'})[0] in (400,422)
            code,redacted=call('/chat',{'prompt':'api_key=synthetic-value','scope':'secret-test'});assert code==200 and redacted['redacted']
            assert all('synthetic-value' not in json.dumps(r) for r in requests)
            print('PASS: authenticated API, origin protection, candidate filtering/editing, approval, scope, multi-turn recall, idempotent ingestion, path rejection, provider redaction')
        finally:
            app.terminate()
            try:app.wait(timeout=5)
            except subprocess.TimeoutExpired:app.kill();app.wait()
            provider.shutdown();provider.server_close()
if __name__=='__main__':main()
