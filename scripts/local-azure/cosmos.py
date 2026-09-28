#!/usr/bin/env python3
"""A Cosmos DB NoSQL REST stand-in for exactly what usnm-ingest's CosmosDocs
calls, so several ingest processes can share state locally (the --state-file
store is one process at a time).

- Entra auth only, like the real account: `authorization:
  type=aad&ver=1.0&sig={jwt}` whose audience is https://cosmos.azure.com and
  which has not expired (shim.py's identity endpoint mints these).
- Items keyed by (container, partition key, id), with quoted etags.
- POST: create (409 if it exists), upsert (x-ms-documentdb-is-upsert), or
  query (`SELECT * FROM c` and `... WHERE ARRAY_CONTAINS(@values, c.field)`).
- PUT: replace, with If-Match (412 on a stale etag, 404 if missing).
- Session tokens are returned; queries are paged with x-ms-continuation.

It checks the client against this reading of the REST API, not against
Cosmos itself: the first run against a real account is still the real test.

Environment:
  COSMOS_PORT  listen port (default 10030)
  COSMOS_FILE  where items persist after every write (default cosmos.json)
  COSMOS_SEED  a usnm-ingest --state-file to start from, if COSMOS_FILE
               does not exist yet
  COSMOS_PAGE  query page size (default 100; 2 exercises continuation)
  COSMOS_429   fraction of requests to throttle with 429 (default 0)
"""

import base64
import json
import os
import random
import re
import socketserver
import sys
import threading
import time
import urllib.parse
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("COSMOS_PORT", "10030"))
FILE = os.environ.get("COSMOS_FILE", "cosmos.json")
PAGE = int(os.environ.get("COSMOS_PAGE", "100"))
P429 = float(os.environ.get("COSMOS_429", "0"))
AUDIENCE = "https://cosmos.azure.com"
QUERY_IN = re.compile(r"SELECT \* FROM c WHERE ARRAY_CONTAINS\(@values, c\.(\w+)\)")
DOCS_PATH = re.compile(r"/dbs/([^/]+)/colls/([^/]+)/docs(?:/([^/?]+))?")

lock = threading.Lock()
items = {}  # (container, pk, id) -> item with system properties
lsn = 0


class Server(ThreadingHTTPServer):
    # HTTPServer.server_bind looks up the host's FQDN, which can take tens
    # of seconds on macOS; the name is never used here.
    def server_bind(self):
        socketserver.TCPServer.server_bind(self)
        self.server_name, self.server_port = self.server_address[:2]


def save():
    rows = [[c, pk, d] for (c, pk, _), d in sorted(items.items())]
    tmp = FILE + ".tmp"
    with open(tmp, "w") as f:
        json.dump({"items": rows, "lsn": lsn}, f, indent=1)
    os.replace(tmp, FILE)


def load():
    global lsn
    if os.path.exists(FILE):
        with open(FILE) as f:
            saved = json.load(f)
        for c, pk, d in saved["items"]:
            items[(c, pk, d["id"])] = d
        lsn = saved["lsn"]
    elif os.environ.get("COSMOS_SEED"):
        with open(os.environ["COSMOS_SEED"]) as f:
            for c, pk, doc, _etag in json.load(f):
                items[(c, pk, doc["id"])] = stamp(doc)
        save()


def stamp(doc):
    global lsn
    lsn += 1
    d = dict(doc)
    d.update(
        {
            "_etag": f'"{uuid.uuid4()}"',
            "_ts": int(time.time()),
            "_rid": base64.b64encode(os.urandom(6)).decode(),
            "_self": "",
        }
    )
    return d


def claims(auth):
    m = re.fullmatch(
        r"type=aad&ver=1\.0&sig=([\w-]+)\.([\w-]+)\.([\w-]*)",
        urllib.parse.unquote(auth or ""),
    )
    if not m:
        return None
    payload = m.group(2) + "=" * (-len(m.group(2)) % 4)
    return json.loads(base64.urlsafe_b64decode(payload))


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def reply(self, status, body=None, headers=()):
        data = json.dumps(body).encode() if body is not None else b""
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("x-ms-session-token", f"0:{lsn}")
        for k, v in headers:
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(data)

    def error(self, status, code, message):
        self.reply(status, {"code": code, "message": message})

    def route(self):
        # Read the body before any early reply, or it is left on the
        # keep-alive connection and parsed as the next request.
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n) if n else b""
        c = claims(self.headers.get("authorization"))
        if not c or c.get("aud") != AUDIENCE or c.get("exp", 0) < time.time():
            return self.error(401, "Unauthorized", "missing, expired or wrong-audience Entra token")
        if self.headers.get("x-ms-version") is None:
            return self.error(400, "BadRequest", "x-ms-version required")
        if P429 and random.random() < P429:
            return self.reply(429, {"code": "TooManyRequests"}, [("x-ms-retry-after-ms", "40")])
        m = DOCS_PATH.fullmatch(urllib.parse.urlparse(self.path).path)
        if not m:
            return self.error(404, "NotFound", "unknown resource")
        _, coll, doc_id = m.groups()
        doc_id = urllib.parse.unquote(doc_id) if doc_id else None
        pk_header = self.headers.get("x-ms-documentdb-partitionkey")
        pk = json.loads(pk_header)[0] if pk_header else None
        with lock:
            return self.dispatch(coll, doc_id, pk, body)

    def dispatch(self, coll, doc_id, pk, body):
        if self.command == "GET" and doc_id:
            d = items.get((coll, pk, doc_id))
            if d is None:
                return self.error(404, "NotFound", "no such item")
            return self.reply(200, d, [("etag", d["_etag"])])
        if self.command == "PUT" and doc_id:
            key = (coll, pk, doc_id)
            current = items.get(key)
            if current is None:
                return self.error(404, "NotFound", "no such item")
            if self.headers.get("if-match") not in (None, current["_etag"]):
                return self.error(412, "PreconditionFailed", "etag mismatch")
            doc = json.loads(body)
            if doc.get("id") != doc_id:
                return self.error(400, "BadRequest", "id mismatch")
            items[key] = stamp(doc)
            save()
            return self.reply(200, items[key], [("etag", items[key]["_etag"])])
        if self.command == "POST" and not doc_id:
            if self.headers.get("x-ms-documentdb-isquery") == "True":
                return self.query(coll, json.loads(body))
            if pk is None:
                return self.error(400, "BadRequest", "partition key header required")
            doc = json.loads(body)
            key = (coll, pk, doc["id"])
            upsert = self.headers.get("x-ms-documentdb-is-upsert") == "True"
            existed = key in items
            if existed and not upsert:
                return self.error(409, "Conflict", "item exists")
            items[key] = stamp(doc)
            save()
            return self.reply(200 if existed else 201, items[key], [("etag", items[key]["_etag"])])
        return self.error(405, "MethodNotAllowed", self.command)

    def query(self, coll, q):
        rows = [d for (c, _, _), d in sorted(items.items()) if c == coll]
        m = QUERY_IN.fullmatch(q["query"])
        if m:
            values = {p["name"]: p["value"] for p in q["parameters"]}["@values"]
            rows = [d for d in rows if d.get(m.group(1)) in values]
        elif q["query"] != "SELECT * FROM c":
            return self.error(400, "BadRequest", f"unsupported query: {q['query']}")
        start = int(self.headers.get("x-ms-continuation") or 0)
        page = rows[start : start + PAGE]
        more = start + PAGE < len(rows)
        headers = [("x-ms-continuation", str(start + PAGE))] if more else []
        return self.reply(200, {"_rid": "", "Documents": page, "_count": len(page)}, headers)

    do_GET = do_PUT = do_POST = route

    def log_message(self, fmt, *args):
        sys.stderr.write(f"{self.command} {self.path} {args[1] if len(args) > 1 else ''}\n")


if __name__ == "__main__":
    load()
    print(f"cosmos http://127.0.0.1:{PORT}/ ({len(items)} items)", flush=True)
    Server(("127.0.0.1", PORT), Handler).serve_forever()
