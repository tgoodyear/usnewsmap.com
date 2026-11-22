#!/usr/bin/env python3
"""Hands Quickwit Blob Storage tokens from the host's Arc identity.

Quickwit 0.9 authenticates to Azure Storage through the azure_identity crate,
which knows the App Service managed identity endpoint (IDENTITY_ENDPOINT +
IDENTITY_HEADER) but not the Arc agent's challenge: on an Arc host its
requests got "unauthorized", so the release's prune step never reached the
index account. This serves the App Service protocol on loopback and answers
each request with a token from the Arc agent, for Blob Storage only.

The release's prune step starts it, runs Quickwit through quickwit-arc.sh
(AZURE_CREDENTIAL_KIND=appservice, pointed here), and stops it afterwards.
Run as a member of the himds group, like the rest of the release.

  USNM_ARC_SHIM_HEADER=<secret> arc-token-shim.py --port-file FILE

It listens on 127.0.0.1 on a free port, writes the port to FILE once it
listens, and refuses requests without the secret in X-IDENTITY-HEADER. No
third-party packages.
"""

import argparse
import hmac
import http.client
import ipaddress
import json
import os
import stat
import sys
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ARC_ENDPOINT = os.environ.get("USNM_ARC_ENDPOINT", "http://localhost:40342/metadata/identity/oauth2/token")
ARC_TOKEN_DIR = os.environ.get("USNM_ARC_TOKEN_DIR", "/var/opt/azcmagent/tokens")
# The only resource handed out: the index account is all Quickwit reaches.
STORAGE = "https://storage.azure.com"


class Refused(Exception):
    def __init__(self, status, message):
        super().__init__(message)
        self.status = status


def challenge_file(realm):
    """The agent's challenge file, checked as usnm-store's ArcIdentity checks it: a .key file
    directly in the agent's token directory, regular, not a link, at most 4 KiB. Anything else
    answering on the port could otherwise name any file."""
    tokens = os.path.realpath(ARC_TOKEN_DIR)
    if os.path.islink(realm):
        raise Refused(502, "the challenge file is a link")
    path = os.path.realpath(realm)
    if not path.endswith(".key") or os.path.dirname(path) != tokens:
        raise Refused(502, "the challenge file isn't one of the agent's")
    try:
        st = os.stat(path)
    except OSError:
        raise Refused(502, "the challenge names a file that can't be read")
    if not stat.S_ISREG(st.st_mode) or st.st_size > 4096:
        raise Refused(502, "the challenge file isn't a small regular file")
    with open(path) as f:
        return f.read().strip()


def loopback_endpoint(endpoint):
    """The agent's endpoint must be plain HTTP on loopback, as usnm-store's ArcIdentity
    requires: the challenge file's secret goes to it in the clear."""
    url = urllib.parse.urlsplit(endpoint)
    host = url.hostname or ""
    if host.lower() == "localhost":
        loopback = True
    else:
        try:
            loopback = ipaddress.ip_address(host).is_loopback
        except ValueError:
            loopback = False
    return url.scheme == "http" and loopback


def basic_realm(header):
    """The realm of a Basic challenge, with or without quotes; None for anything else."""
    prefix = "basic realm="
    if not header.lower().startswith(prefix):
        return None
    realm = header[len(prefix):].strip()
    if len(realm) >= 2 and realm[0] == realm[-1] == '"':
        realm = realm[1:-1]
    return realm or None


def arc_get(path, headers):
    url = urllib.parse.urlsplit(ARC_ENDPOINT)
    # The agent is on loopback: a direct connection, never through a proxy.
    conn = http.client.HTTPConnection(url.hostname, url.port or 80, timeout=30)
    try:
        conn.request("GET", path, headers=headers)
        rsp = conn.getresponse()
        return rsp.status, rsp.getheader("WWW-Authenticate") or "", rsp.read()
    finally:
        conn.close()


def arc_token(resource):
    url = urllib.parse.urlsplit(ARC_ENDPOINT)
    query = urllib.parse.urlencode({"api-version": "2020-06-01", "resource": resource})
    path = "%s?%s" % (url.path, query)
    status, auth, _ = arc_get(path, {"Metadata": "true"})
    realm = basic_realm(auth)
    if status != 401 or realm is None:
        raise Refused(502, "no challenge from the Arc agent (status %d)" % status)
    key = challenge_file(realm)
    status, _, body = arc_get(path, {"Metadata": "true", "Authorization": "Basic " + key})
    if status != 200:
        raise Refused(502, "the Arc agent refused the token request (status %d; is this user in himds?)" % status)
    try:
        token = json.loads(body)
        expires_on = int(token["expires_on"])
        access_token = token["access_token"]
    except (ValueError, KeyError, TypeError):
        raise Refused(502, "the Arc agent's token response isn't a token")
    # azure_identity reads expires_on as a string of epoch seconds.
    return {
        "access_token": access_token,
        "expires_on": str(expires_on),
        "resource": token.get("resource", resource),
        "token_type": token.get("token_type", "Bearer"),
    }


class Handler(BaseHTTPRequestHandler):
    secret = ""

    def reply(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        try:
            given = self.headers.get("X-IDENTITY-HEADER", "")
            if not hmac.compare_digest(given.encode(), self.secret.encode()):
                raise Refused(401, "missing or wrong X-IDENTITY-HEADER")
            query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
            resource = (query.get("resource") or [""])[0]
            if resource.rstrip("/") != STORAGE:
                raise Refused(400, "only %s tokens are handed out, not %r" % (STORAGE, resource))
            self.reply(200, arc_token(STORAGE))
        except Refused as e:
            self.log_message("refused: %s", e)
            self.reply(e.status, {"error": str(e)})
        except OSError as e:
            self.log_message("Arc agent unreachable: %s", e)
            self.reply(502, {"error": "the Arc agent is unreachable"})

    def log_message(self, fmt, *args):
        sys.stderr.write("arc-token-shim: %s\n" % (fmt % args))


class Server(ThreadingHTTPServer):
    daemon_threads = True

    # HTTPServer.server_bind looks up the host's FQDN; the name is never used.
    def server_bind(self):
        self.socket.bind(self.server_address)
        self.server_address = self.socket.getsockname()


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--port-file", required=True)
    args = parser.parse_args()
    secret = os.environ.get("USNM_ARC_SHIM_HEADER", "")
    if len(secret) < 16:
        sys.exit("arc-token-shim: USNM_ARC_SHIM_HEADER must be a secret of at least 16 characters")
    if not loopback_endpoint(ARC_ENDPOINT):
        sys.exit("arc-token-shim: the Arc agent's endpoint must be http on loopback, not %s" % ARC_ENDPOINT)
    Handler.secret = secret
    server = Server(("127.0.0.1", 0), Handler)
    tmp = args.port_file + ".tmp"
    with open(tmp, "w") as f:
        f.write("%d\n" % server.server_address[1])
    os.replace(tmp, args.port_file)
    server.serve_forever()


if __name__ == "__main__":
    main()
