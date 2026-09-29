import http.server,json,pathlib
root=pathlib.Path(__file__).resolve().parent
assets=json.loads((root/'assets.json').read_text())
class Handler(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  name=self.path.split('?')[0]
  if name.startswith('/asset/'):
   p=pathlib.Path(assets.get(name[7:],'NONEXISTENT')); kind='application/octet-stream'
  else:
   name=name.lstrip('/') or 'index.html'
   if name not in ['index.html','worker.mjs','run.mjs','jit.mjs']:self.send_error(404);return
   p=root/name;kind='text/html' if name.endswith('.html') else 'text/javascript'
  data=p.read_bytes();self.send_response(200);self.send_header('Content-Type',kind);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
 def do_POST(self):
  if self.path!='/events':self.send_error(404);return
  body=self.rfile.read(int(self.headers['Content-Length'])).decode()
  with (root/'browser-events.jsonl').open('a') as f:f.write(body)
  for line in body.splitlines():
   e=json.loads(line)
   if e['type']=='serial':
    with (root/'browser-console.log').open('a') as f:f.write(e['data'])
   elif e['type'] in ['result','error']:(root/'browser-result.json').write_text(json.dumps(e,indent=2))
   elif e['type']=='progress':(root/'browser-progress.json').write_text(json.dumps(e,indent=2))
  self.send_response(200);self.end_headers();self.wfile.write(b'ok')
 def log_message(self,*a):pass
http.server.ThreadingHTTPServer(('127.0.0.1',8791),Handler).serve_forever()
