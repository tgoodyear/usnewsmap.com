#!/usr/bin/env python3
"""Read-only Logs API client with bounded, DNS-refreshing frontend retries."""

import argparse
import contextlib
import http.client
import json
import pathlib
import random
import re
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

AUDIENCE = "https://api.loganalytics.io"


class QueryError(RuntimeError):
    def __init__(self, message, history=None):
        super().__init__(message)
        self.history = history or []


def retryable(status, body):
    return (status == 429 or 500 <= status <= 599
            or (status == 403 and isinstance(body, dict)
                and isinstance(body.get("error"), dict)
                and body["error"].get("code") == "InsufficientAccessError"))


def access_token(identity="az", subscription=None):
    if identity == "az":
        args = ["az", "account", "get-access-token", "--resource", AUDIENCE,
                "--only-show-errors", "-o", "json"]
        if subscription:
            args += ["--subscription", subscription]
        result = subprocess.run(args, capture_output=True, text=True, timeout=30)
        if result.returncode:
            raise QueryError("Azure CLI token acquisition failed; check the existing login")
        return json.loads(result.stdout)["accessToken"]
    url = ("http://127.0.0.1:40342/metadata/identity/oauth2/token?"
           + urllib.parse.urlencode({"api-version": "2020-06-01", "resource": AUDIENCE}))
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    request = urllib.request.Request(url, headers={"Metadata": "true"})
    try:
        with opener.open(request, timeout=10):
            raise QueryError("Arc endpoint did not issue the expected challenge")
    except urllib.error.HTTPError as error:
        challenge = error.headers.get("WWW-Authenticate", "")
        if error.code != 401 or not challenge.startswith("Basic realm="):
            raise QueryError("Arc identity challenge failed") from None
    path = pathlib.Path(challenge[len("Basic realm="):].strip('"'))
    if path.parent != pathlib.Path("/var/opt/azcmagent/tokens"):
        raise QueryError("Unexpected Arc challenge path")
    secret = path.read_text().strip()
    request.add_header("Authorization", "Basic " + secret)
    with opener.open(request, timeout=20) as response:
        token = json.load(response)["access_token"]
    return token


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


@contextlib.contextmanager
def absolute_timeout(seconds):
    """CLI runs on the main thread on macOS/Linux; bound DNS and streaming reads too."""
    def expired(signum, frame):
        raise TimeoutError("Logs deadline expired")

    previous = signal.getsignal(signal.SIGALRM)
    signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        yield
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


def query(workspace, kql, timespan="P1D", *, token=None, identity="az",
          subscription=None, attempts=6, budget=60):
    if not re.fullmatch(r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}", workspace):
        raise QueryError("workspace must be its customer GUID, not an ARM resource ID")
    if token is None:
        token = access_token(identity, subscription)
    history = []
    try:
        with absolute_timeout(budget):
            return _query(workspace, kql, timespan, token, attempts, budget, history)
    except TimeoutError:
        raise QueryError("Logs query absolute deadline expired", history) from None


def _query(workspace, kql, timespan, token, attempts, budget, history):
    url = f"{AUDIENCE}/v1/workspaces/{workspace}/query"
    data = json.dumps({"query": kql, "timespan": timespan}).encode()
    deadline = time.monotonic() + budget
    for attempt in range(1, attempts + 1):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        request = urllib.request.Request(url, data=data, headers={
            "Authorization": "Bearer " + token, "Content-Type": "application/json",
            "x-ms-client-request-id": str(uuid.uuid4()),
        })
        # urllib opens a fresh connection; no connection pool or DNS/IP pin survives attempts.
        opener = urllib.request.build_opener(NoRedirect())
        try:
            response = opener.open(request, timeout=min(20, remaining))
        except urllib.error.HTTPError as error:
            response = error
        except (urllib.error.URLError, OSError, http.client.HTTPException):
            history.append({"attempt": attempt, "status": "transport-error",
                            "via": "", "requestId": None})
            raise QueryError("Logs query transport failed", history) from None
        status = response.status
        via = re.sub(r"[\r\n]", " ", response.headers.get("via", ""))[:240]
        record = {"attempt": attempt, "status": status, "via": via,
                  "requestId": response.headers.get("x-ms-request-id")}
        history.append(record)
        print(f"Logs attempt {attempt}/{attempts}: HTTP {status}; via={via or '-'}",
              file=sys.stderr)
        try:
            with response:
                raw = response.read()
        except (OSError, http.client.HTTPException):
            record["error"] = "response-read-failed"
            raise QueryError("Logs response read failed", history) from None
        else:
            try:
                body = json.loads(raw)
            except (ValueError, UnicodeDecodeError):
                body = None
        if 200 <= status < 300:
            if (not isinstance(body, dict) or "error" in body
                    or not isinstance(body.get("tables"), list) or not body["tables"]):
                raise QueryError("Logs response missing tables or contains a partial error", history)
            return body, history
        if not retryable(status, body):
            raise QueryError(f"Logs query denied/failed (HTTP {status}; non-retryable)", history)
        delay = random.uniform(3, 6)
        if attempt == attempts or time.monotonic() + delay >= deadline:
            break
        time.sleep(delay)
    raise QueryError(f"Logs query exhausted bounded retry ({len(history)} attempts)", history)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", required=True)
    parser.add_argument("--query", required=True)
    parser.add_argument("--timespan", default="P1D")
    parser.add_argument("--identity", choices=["az", "arc"], default="az")
    parser.add_argument("--subscription")
    args = parser.parse_args()
    try:
        body, _ = query(args.workspace, args.query, args.timespan,
                        identity=args.identity, subscription=args.subscription)
        print(json.dumps(body))
    except (QueryError, OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        # Authentication/HTTP exception payloads may contain credentials; never dump them.
        print(str(error) if isinstance(error, QueryError) else "Logs client failed",
              file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
