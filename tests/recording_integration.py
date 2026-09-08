#!/usr/bin/env python3
"""RELEASE GATE: actual compiled Rust server + synthetic loopback provider.
Not executed in the source-delivery environment if cargo/binary is unavailable.
No paid provider calls. Exercises durable admission, replay, context and restart.
"""
import json, os, socket, sqlite3, subprocess, tempfile, threading, time, urllib.request, urllib.error, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
received=[]
hold=threading.Event();started=threading.Event()
class Provider(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_POST(self):
        data=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        extraction=data['messages'][0]['content'].startswith('Extract at most')
        if extraction:text='[]'
        else:
            received.append(data)
            prompt=data['messages'][-1]['content'];started.set()
            if prompt=='hold for restart':hold.wait(15)
            if prompt=='simulate provider failure':
                self.send_response(503);self.end_headers();return
            text='Synthetic durable answer'
        payload=json.dumps({'choices':[{'message':{'content':text}}]}).encode()
        try:
            self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(payload)));self.end_headers();self.wfile.write(payload)
        except (BrokenPipeError,ConnectionResetError):pass

def port():
    with socket.socket() as sock:sock.bind(('127.0.0.1',0));return sock.getsockname()[1]
def main():
    binary=ROOT/'target/debug/harness'
    if not binary.is_file():raise SystemExit('NOT RUN: build first with cargo build --locked')
    provider=ThreadingHTTPServer(('127.0.0.1',0),Provider);threading.Thread(target=provider.serve_forever,daemon=True).start()
    with tempfile.TemporaryDirectory(prefix='recording-integration-') as tmp:
        db=Path(tmp)/'fixture.db';server_port=port();token='synthetic-'+'x'*40
        env={**os.environ,'HARNESS_DB':str(db),'HARNESS_AUTH_TOKEN':token,'HARNESS_API_KEY':'synthetic','HARNESS_BASE_URL':f'http://127.0.0.1:{provider.server_port}','HARNESS_ADDR':f'127.0.0.1:{server_port}','HARNESS_MODEL':'synthetic-model'}
        app=None
        def call(path,body=None,auth=True,origin=None):
            headers={'Content-Type':'application/json'}
            if auth:headers['Authorization']='Bearer '+token
            if origin:headers['Origin']=origin
            req=urllib.request.Request(f'http://127.0.0.1:{server_port}'+path,data=None if body is None else json.dumps(body).encode(),headers=headers)
            try:
                with urllib.request.urlopen(req,timeout=10) as response:return response.status,json.load(response)
            except urllib.error.HTTPError as e:return e.code,json.loads(e.read())
        def start():
            process=subprocess.Popen([str(binary)],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            for _ in range(100):
                if process.poll() is not None:raise AssertionError('Server exited during startup')
                try:
                    if call('/memory/status')[0]==200:return process
                except urllib.error.URLError:pass
                time.sleep(.1)
            process.kill();process.wait();raise AssertionError('Server startup timeout')
        def submit(prompt):
            body={'request_id':str(uuid.uuid4()),'session_id':str(uuid.uuid4()),'scope':'global','prompt':prompt}
            code,receipt=call('/chat/submit',body);assert code in (200,202),(code,receipt);return body,receipt
        def wait(request,state):
            for _ in range(150):
                code,receipt=call('/chat/requests/'+request)
                if code==200 and receipt['state']==state:return receipt
                time.sleep(.05)
            raise AssertionError(('receipt timeout',state,receipt))
        try:
            app=start()
            first,_=submit('I prefer Rust')
            done=wait(first['request_id'],'complete');assert done['response']=='Synthetic durable answer'
            calls=len(received);assert call('/chat/submit',first)[0]==200;time.sleep(.3);assert len(received)==calls
            changed={**first,'prompt':'Changed content'};assert call('/chat/submit',changed)[0]==409
            detail=call('/chat/requests/'+first['request_id']+'/context')[1]
            assert detail['context']['provider_messages']==received[0]['messages']
            assert detail['context']['model']==received[0]['model']
            assert [e['kind'] for e in detail['events']][:4]==['captured','generation_started','context_saved','answer_saved']
            assert call('/chat/requests/'+first['request_id'],auth=False)[0]==401
            assert call('/chat/requests/'+first['request_id']+'/context',origin='https://untrusted.invalid')[0]==403
            assert call('/sessions',auth=False)[0]==401
            failed,_=submit('simulate provider failure');wait(failed['request_id'],'failed')
            history=call('/sessions/'+failed['session_id']+'/messages')[1];assert len(history['messages'])==1;assert history['messages'][0]['content']=='simulate provider failure'
            calls=len(received);call('/chat/submit',failed);time.sleep(.3);assert len(received)==calls
            # Crash after the provider was invoked. No auto-retry of a potentially billed turn.
            started.clear();pending,_=submit('hold for restart');assert started.wait(5)
            with sqlite3.connect(db) as c:
                assert c.execute('SELECT content FROM messages WHERE id=?',(pending['request_id'],)).fetchone()[0]=='hold for restart'
                assert c.execute('SELECT context_json FROM chat_receipts WHERE request_id=?',(pending['request_id'],)).fetchone()[0] is not None
            app.kill();app.wait(timeout=5);before=len(received);hold.set();app=start()
            interrupted=wait(pending['request_id'],'interrupted');assert interrupted['response'] is None
            call('/chat/submit',pending);time.sleep(.5);assert len(received)==before
            # Recovery can find sessions without browser-local transcript storage.
            sessions=call('/sessions')[1]['sessions'];assert {s['id'] for s in sessions}>={first['session_id'],failed['session_id'],pending['session_id']}
            with sqlite3.connect(db) as c:
                assert c.execute('PRAGMA integrity_check').fetchone()[0]=='ok';assert c.execute('PRAGMA foreign_key_check').fetchall()==[]
            print('PASS: real HTTP admission, idempotency, context equality, auth/origin, provider failure, SIGKILL/restart, no generation replay, session recovery, SQLite integrity')
        finally:
            hold.set()
            if app and app.poll() is None:
                app.terminate()
                try:app.wait(timeout=5)
                except subprocess.TimeoutExpired:app.kill();app.wait()
            provider.shutdown();provider.server_close()
if __name__=='__main__':main()
