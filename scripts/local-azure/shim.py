#!/usr/bin/env python3
"""Local stand-ins for the managed identity endpoint and Blob Storage auth.

- A managed identity endpoint (IDENTITY_ENDPOINT + IDENTITY_HEADER, API
  version 2019-08-01) on :10020 that mints unsigned JWTs for the requested
  resource, so usnm-store's ManagedIdentity credential runs as it does in
  Container Apps.
- A plain-HTTP proxy on :10010 to Azurite's HTTPS endpoint on :10000.
  Azurite only checks bearer tokens with `--oauth basic`, which needs HTTPS,
  and usnm-store's rustls client trusts only public roots. The proxy carries
  the token over loopback HTTP; Azurite still checks its audience, issuer
  and expiry, so a Cosmos token is refused for Blob, as in Azure.

Run by scripts/local-azure/up.sh. No third-party packages.
"""

import base64
import http.client
import json
import os
import socketserver
import ssl
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HEADER = os.environ.get("SHIM_IDENTITY_HEADER", "local")
TENANT = "00000000-0000-0000-0000-000000000001"
AZURITE = ("127.0.0.1", 10000)
# Azurite's certificate is self-signed and only ever on loopback.
TLS = ssl._create_unverified_context()
HOP = {"connection", "keep-alive", "transfer-encoding", "host", "proxy-connection"}


class Server(ThreadingHTTPServer):
    # HTTPServer.server_bind looks up the host's FQDN, which can take tens
    # of seconds on macOS; the name is never used here.
    def server_bind(self):
        socketserver.TCPServer.server_bind(self)
        self.server_name, self.server_port = self.server_address[:2]


def b64(d):
    return base64.urlsafe_b64encode(json.dumps(d).encode()).rstrip(b"=").decode()


def mint(resource):
    now = int(time.time())
    claims = {
        "aud": resource.rstrip("/"),
        "iss": f"https://sts.windows.net/{TENANT}/",
        "iat": now,
        "nbf": now,
        "exp": now + 3600,
        "oid": "11111111-1111-1111-1111-111111111111",
        "tid": TENANT,
    }
    # Azurite's basic OAuth mode does not verify signatures.
    return f"{b64({'alg': 'RS256', 'typ': 'JWT'})}.{b64(claims)}.c2ln", now + 3600


class Identity(BaseHTTPRequestHandler):
    def do_GET(self):
        q = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        if self.headers.get("X-IDENTITY-HEADER") != HEADER or "resource" not in q:
            self.send_response(401)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        token, exp = mint(q["resource"][0])
        body = json.dumps(
            {
                "access_token": token,
                "expires_on": str(exp),
                "resource": q["resource"][0],
                "token_type": "Bearer",
            }
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class Proxy(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def forward(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n) if n else None
        headers = {k: v for k, v in self.headers.items() if k.lower() not in HOP}
        conn = http.client.HTTPSConnection(*AZURITE, context=TLS, timeout=300)
        conn.request(self.command, self.path, body=body, headers=headers)
        resp = conn.getresponse()
        data = resp.read()
        self.send_response(resp.status, resp.reason)
        for k, v in resp.getheaders():
            if k.lower() not in HOP and k.lower() != "content-length":
                self.send_header(k, v)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(data)
        conn.close()
        sys.stderr.write(f"{self.command} {self.path.split('?')[0]} {resp.status}\n")

    do_GET = do_PUT = do_HEAD = do_DELETE = do_POST = forward

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    for cls, port in ((Identity, 10020), (Proxy, 10010)):
        server = Server(("127.0.0.1", port), cls)
        threading.Thread(target=server.serve_forever, daemon=True).start()
    print("identity http://127.0.0.1:10020/msi/token, blob http://127.0.0.1:10010", flush=True)
    threading.Event().wait()
