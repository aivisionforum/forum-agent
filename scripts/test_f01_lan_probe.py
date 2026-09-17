"""Real TLS boundary tests for the disposable F01 probe; no LAN listener."""
import http.client
import importlib.util
import json
from pathlib import Path
import ssl
import socket
from unittest import mock
import tempfile
import threading
import time
import unittest

spec = importlib.util.spec_from_file_location('f01_lan_probe',Path(__file__).with_name('f01_lan_probe.py'))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class LanProbeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.directory = Path(cls.temp.name)/'tls'
        probe.prepare_certificates(cls.directory,'127.0.0.1')
        cls.server = probe.ProbeServer('127.0.0.1',0,cls.directory,60)
        cls.worker = threading.Thread(target=cls.server.serve_forever)
        cls.worker.start()
        cls.context = ssl.create_default_context(cafile=str(cls.directory/'forum-f01-ca.crt'))
        cls.origin = f'https://127.0.0.1:{cls.server.server_port}'

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.worker.join(timeout=3)
        cls.server.server_close()
        cls.temp.cleanup()

    def request(self,path,body=None,headers=None):
        conn = http.client.HTTPSConnection('127.0.0.1',self.server.server_port,context=self.context,timeout=3)
        try:
            conn.request('GET' if body is None else 'POST',path,body=body,headers=headers or {})
            response = conn.getresponse()
            data = response.read()
            return response.status,dict(response.getheaders()),data
        finally:
            conn.close()

    def pair(self,token=None,origin=None):
        return self.request('/pair',json.dumps({'token':token or self.server.token}),{'Content-Type':'application/json','Origin':origin or self.origin})

    def test_private_routes_require_pairing_and_do_not_expose_old_control(self):
        self.assertEqual(self.request('/sample')[0],401)
        self.assertEqual(self.request('/control')[0],404)
        self.assertEqual(self.request('/../server.key')[0],404)
        self.assertEqual(self.pair(token='wrong')[0],401)
        self.assertEqual(self.pair(origin='https://example.invalid')[0],403)

    def test_trusted_tls_pair_cookie_then_expiry(self):
        code,headers,_ = self.pair()
        self.assertEqual(code,200)
        cookie = headers['Set-Cookie']
        for attribute in ['Secure','HttpOnly','SameSite=Strict']:
            self.assertIn(attribute,cookie)
        code,_,data = self.request('/sample',headers={'Cookie':cookie.split(';')[0]})
        self.assertEqual(code,200)
        self.assertIn('Synthetic test:',json.loads(data)['translation'])
        expiry = self.server.expires
        try:
            self.server.expires = time.monotonic()-1
            for request in [lambda: self.request('/sample',headers={'Cookie':cookie.split(';')[0]}), self.pair]:
                try:
                    self.assertEqual(request()[0],401)
                except (OSError,http.client.HTTPException):
                    pass  # The absolute service deadline may close TLS first.
        finally:
            self.server.expires = expiry

    def test_token_is_absent_from_public_html_and_ca_private_key_deleted(self):
        code,headers,body = self.request('/')
        self.assertEqual(code,200)
        self.assertNotIn(self.server.token.encode(),body)
        self.assertEqual(headers['Cache-Control'],'no-store')
        self.assertFalse((self.directory/'ca.key').exists())
        self.assertEqual((self.directory/'server.key').stat().st_mode & 0o777,0o600)
        with self.assertRaises(FileExistsError):
            probe.prepare_certificates(self.directory,'127.0.0.1')

    def test_slow_request_cannot_keep_expired_service_running(self):
        server = probe.ProbeServer('127.0.0.1',0,self.directory,1)
        def serve():
            with server:
                while time.monotonic() < server.expires:
                    server.handle_request()
        worker = threading.Thread(target=serve)
        worker.start()
        client = self.context.wrap_socket(socket.create_connection(server.server_address),server_hostname='127.0.0.1')
        try:
            client.sendall(b'GET / HTTP/1.1\r\nX-Slow: ')
            deadline = time.monotonic()+2
            while time.monotonic() < deadline and worker.is_alive():
                try:
                    client.sendall(b'a')
                except OSError:
                    break
                time.sleep(0.04)
        finally:
            client.close()
            worker.join(timeout=2)
        self.assertFalse(worker.is_alive())
        self.assertEqual(server.socket.fileno(),-1)

    def test_certificate_failure_removes_only_new_private_keys(self):
        directory = Path(self.temp.name)/'failed-cert'
        def fail(*args,**kwargs):
            (directory/'ca.key').write_text('private')
            (directory/'server.key').write_text('private')
            raise RuntimeError('simulated openssl failure')
        with mock.patch.object(probe.subprocess,'run',side_effect=fail):
            with self.assertRaises(RuntimeError):
                probe.prepare_certificates(directory,'127.0.0.1')
        self.assertFalse((directory/'ca.key').exists())
        self.assertFalse((directory/'server.key').exists())
        (directory/'server.key').write_text('existing-owned-by-user')
        with self.assertRaises(FileExistsError):
            probe.prepare_certificates(directory,'127.0.0.1')
        self.assertEqual((directory/'server.key').read_text(),'existing-owned-by-user')


if __name__ == '__main__':
    unittest.main()
