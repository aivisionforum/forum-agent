#!/usr/bin/env python3
"""F01 disposable HTTPS feasibility probe. Synthetic content; no meeting APIs.

Runs with the bundled standalone Python. Creates a unique local test CA but
never installs trust or changes system configuration. Ctrl-C stops the server.
"""
from __future__ import annotations
import argparse
import hashlib
import http.cookies
import http.server
import ipaddress
import json
import os
from pathlib import Path
import secrets
import ssl
import socket
import threading
import subprocess
import time

PAGE = '''<!doctype html><html lang="zh-CN"><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>Forum 局域网验证</title>
<style>body{font:18px system-ui;max-width:680px;margin:10vh auto;padding:24px;background:#eef4f8;color:#17324a}section{padding:24px;background:white;border-radius:16px}p{line-height:1.7}small{color:#567}</style>
<section><small>AI VISION FORUM · F01 设备验证</small><h1>局域网连接测试</h1>
<p id="state">正在检查安全连接和配对…</p><div id="sample"></div>
<p><small>此页只显示合成句子，不接收麦克风，也不读取真实会议。</small></p></section>
<script src="/probe.js"></script></html>'''
JS = '''(async()=>{const state=document.querySelector('#state'),sample=document.querySelector('#sample');
try{if(!window.isSecureContext)throw Error('浏览器未确认安全上下文，请安装并信任测试证书');
const token=new URLSearchParams(location.hash.slice(1)).get('pair');history.replaceState(null,'',location.pathname);
if(token){const r=await fetch('/pair',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({token})});if(!r.ok)throw Error('配对失败或已过期');}
async function tick(){const r=await fetch('/sample',{cache:'no-store'});if(!r.ok)throw Error('请使用本机终端显示的完整配对链接');const d=await r.json();state.textContent='HTTPS 与配对通过 · 更新 '+d.tick;sample.replaceChildren();for(const text of [d.source,d.translation]){const p=document.createElement('p');p.textContent=text;sample.append(p);}}
await tick();const timer=setInterval(()=>tick().catch(e=>{clearInterval(timer);state.textContent=e.message}),1000);
}catch(e){state.textContent=e.message;}})();'''


def prepare_certificates(directory: Path, host: str) -> None:
    directory.mkdir(mode=0o700, parents=False, exist_ok=False)
    config = directory / 'openssl.cnf'
    config.write_text(f'''[req]
distinguished_name=dn
prompt=no
[dn]
CN=Forum F01 temporary test CA
[ca]
basicConstraints=critical,CA:TRUE,pathlen:0
keyUsage=critical,keyCertSign,cRLSign
subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid:always
[server]
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid:always
subjectAltName=IP:{host},IP:127.0.0.1
''')
    commands = [
        ['req','-x509','-newkey','rsa:2048','-nodes','-sha256','-days','2','-config',str(config),'-extensions','ca','-keyout',str(directory/'ca.key'),'-out',str(directory/'forum-f01-ca.crt')],
        ['req','-newkey','rsa:2048','-nodes','-sha256','-subj','/CN=Forum F01 LAN probe','-keyout',str(directory/'server.key'),'-out',str(directory/'server.csr')],
        ['x509','-req','-sha256','-days','2','-in',str(directory/'server.csr'),'-CA',str(directory/'forum-f01-ca.crt'),'-CAkey',str(directory/'ca.key'),'-CAcreateserial','-extfile',str(config),'-extensions','server','-out',str(directory/'server.crt')],
    ]
    try:
        for args in commands:
            subprocess.run(['/usr/bin/openssl',*args],check=True,capture_output=True,timeout=15)
        os.chmod(directory/'server.key',0o600)
    except BaseException:
        # This directory was created by this invocation; never remove files in
        # a caller's pre-existing directory when mkdir fails.
        (directory/'server.key').unlink(missing_ok=True)
        raise
    finally:
        (directory/'ca.key').unlink(missing_ok=True)



class ProbeServer(http.server.HTTPServer):
    def __init__(self, host: str, port: int, directory: Path, lifetime: int):
        self.token = secrets.token_urlsafe(32)
        self.sessions: set[str] = set()
        self.expires = time.monotonic() + lifetime
        self.started = time.monotonic()
        self.observations: list[dict] = []
        self.request_timer = None
        super().__init__((host, port), Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        context.load_cert_chain(directory/'server.crt',directory/'server.key')
        self.socket = context.wrap_socket(self.socket,server_side=True,do_handshake_on_connect=False)
        self.timeout = 0.5

    def get_request(self):
        connection, address = self.socket.accept()
        budget = max(0.001,min(5.0,self.expires-time.monotonic()))
        # Socket idle timeouts alone permit indefinitely slow headers/bodies.
        # This timer bounds the whole handshake + request, including slow drip.
        def expire():
            try:
                connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            connection.close()
        self.request_timer = threading.Timer(budget,expire)
        self.request_timer.daemon = True
        self.request_timer.start()
        connection.settimeout(min(2,budget))
        try:
            connection.do_handshake()
        except BaseException:
            self.request_timer.cancel()
            connection.close()
            raise
        return connection,address

    def shutdown_request(self,request):
        if self.request_timer is not None:
            self.request_timer.cancel()
            self.request_timer = None
        super().shutdown_request(request)

    def server_close(self):
        if self.request_timer is not None:
            self.request_timer.cancel()
        super().server_close()

    def handle_error(self,*_):
        pass  # A timed-out/abandoned client contains no diagnostic value.


class Handler(http.server.BaseHTTPRequestHandler):
    server: ProbeServer
    def log_message(self, *_):
        pass  # No tokens, cookies, client headers or request paths in logs.

    def reply(self, code: int, body: bytes = b'', mime='application/json', cookie=None):
        self.send_response(code)
        self.send_header('Content-Type',mime)
        self.send_header('Content-Length',str(len(body)))
        self.send_header('Cache-Control','no-store')
        self.send_header('Referrer-Policy','no-referrer')
        self.send_header('X-Content-Type-Options','nosniff')
        self.send_header('Content-Security-Policy',"default-src 'none'; script-src 'self'; connect-src 'self'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'")
        if cookie:
            self.send_header('Set-Cookie',cookie)
        self.end_headers()
        self.wfile.write(body)

    def authorized(self):
        try:
            cookie = http.cookies.SimpleCookie(self.headers.get('Cookie',''))
            value = cookie.get('forum_probe')
            return value is not None and value.value in self.server.sessions and time.monotonic() < self.server.expires
        except http.cookies.CookieError:
            return False

    def do_GET(self):
        if self.path == '/':
            self.reply(200,PAGE.encode(),'text/html; charset=utf-8')
        elif self.path == '/probe.js':
            self.reply(200,JS.encode(),'text/javascript; charset=utf-8')
        elif self.path == '/sample':
            if not self.authorized():
                self.reply(401); return
            value = {'tick':int(time.monotonic()-self.server.started),'source':'合成测试：今天不发布新版本。','translation':'Synthetic test: We will not release a new version today.'}
            self.reply(200,json.dumps(value,ensure_ascii=False).encode())
        else:
            self.reply(404)

    def do_POST(self):
        if self.path != '/pair':
            self.reply(404); return
        # JS sends same-origin JSON; cross-origin simple requests cannot pair.
        host,port = self.server.server_address
        if self.headers.get('Origin') != f'https://{host}:{port}':
            self.reply(403); return
        try:
            count = int(self.headers.get('Content-Length','0'))
            if not 0 < count <= 512 or self.headers.get('Content-Type') != 'application/json':
                self.reply(400); return
            value = json.loads(self.rfile.read(count))
            token = value.get('token') if isinstance(value,dict) else None
            valid = isinstance(token,str) and secrets.compare_digest(token,self.server.token)
        except (ValueError,UnicodeError):
            self.reply(400); return
        if not valid or time.monotonic() >= self.server.expires:
            self.reply(401); return
        if len(self.server.sessions) >= 16:
            self.reply(429); return
        session = secrets.token_urlsafe(32)
        self.server.sessions.add(session)
        self.server.observations.append({'paired_at_seconds':round(time.monotonic()-self.server.started,1),'client_ip':self.client_address[0]})
        self.reply(200,b'{}',cookie=f'forum_probe={session}; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=1200')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bind',required=True,help='An explicit RFC1918 LAN IPv4 address (or 127.0.0.1 for local tests)')
    parser.add_argument('--output',required=True,type=Path,help='New private directory; never overwritten')
    parser.add_argument('--port',type=int,default=0)
    parser.add_argument('--lifetime',type=int,default=1200)
    args = parser.parse_args()
    address = ipaddress.ip_address(args.bind)
    allowed = address == ipaddress.ip_address('127.0.0.1') or any(address in ipaddress.ip_network(net) for net in ['10.0.0.0/8','172.16.0.0/12','192.168.0.0/16'])
    if address.version != 4 or not allowed or not 0 <= args.port <= 65535 or not 1 <= args.lifetime <= 1200:
        parser.error('Requires explicit private IPv4, valid port and 1..1200 seconds lifetime')
    directory = args.output.absolute()
    prepare_certificates(directory,args.bind)
    try:
        with ProbeServer(args.bind,args.port,directory,args.lifetime) as server:
            ca_der = ssl.PEM_cert_to_DER_cert((directory/'forum-f01-ca.crt').read_text())
            print(json.dumps({'url':f'https://{args.bind}:{server.server_port}/#pair={server.token}','ca':str(directory/'forum-f01-ca.crt'),'ca_sha256':hashlib.sha256(ca_der).hexdigest(),'expires_in_seconds':args.lifetime},ensure_ascii=False),flush=True)
            try:
                while time.monotonic() < server.expires:
                    server.handle_request()
            except KeyboardInterrupt:
                pass
            finally:
                (directory/'report.json').write_text(json.dumps({'scope':'synthetic F01 LAN feasibility only','observations':server.observations,'external_devices_passed':'requires manual confirmation','stopped':True},indent=2))
    finally:
        (directory/'server.key').unlink(missing_ok=True)


if __name__ == '__main__':
    main()
