"""One replica of a job execution does a one-off command's work (mixed.py, american_stories.py).

The job starts several replicas. The first to take `<prefix>.run/<execution>.lock` works and renews it;
a replica retried under the same name takes its own lock back; the others wait for the finished marker,
taking over a lock that goes stale (its owner died; JAOCR_AUDIT_LOCK_MINUTES, 60). A replica of an
execution that finished exits at once.
"""

from __future__ import annotations

import json
import os
import time
from datetime import datetime, timedelta, timezone

import jaocr
import quality

RENEW_SECONDS = 300  # how often the working replica renews its lock
POLL_SECONDS = 30  # how often a waiting replica checks on it


class Solo:
    def __init__(self, curated, prefix: str, label: str, run: str | None = None):
        self.curated, self.label = curated, label
        self.owner = os.environ.get("CONTAINER_APP_REPLICA_NAME") or os.environ.get("HOSTNAME") or "local"
        self.run = run or quality.run_name(self.owner)
        if not jaocr.SAFE_SEGMENT.match(self.run):
            raise ValueError(f"unsafe run name: {self.run!r}")
        self.lock, self.finished = f"{prefix}.run/{self.run}.lock", f"{prefix}.run/{self.run}.finished"
        self.stale = timedelta(minutes=float(os.environ.get("JAOCR_AUDIT_LOCK_MINUTES", "60")))
        self.last = time.time()

    def lease(self) -> bytes:
        return json.dumps({"owner": self.owner, "at": datetime.now(timezone.utc).isoformat()}).encode()

    def start(self) -> bool:
        """Whether this replica does the work (False: another did or does)."""
        c = self.curated
        while not (c.create(self.lock, self.lease()) or c.renew(self.lock, self.lease(), self.owner)
                   or c.take_over(self.lock, self.lease(), self.stale)):
            if c.exists(self.finished):
                jaocr.log(f"{self.label} written by another replica", run=self.run)
                return False
            time.sleep(POLL_SECONDS)
        if c.exists(self.finished):  # a retried replica of an execution that finished
            jaocr.log(f"{self.label} already finished", run=self.run)
            return False
        self.last = time.time()
        return True

    def keep(self) -> None:
        """Renew the lock now and then; raise if another replica took it over."""
        if time.time() - self.last >= RENEW_SECONDS:
            if not self.curated.renew(self.lock, self.lease(), self.owner):
                raise RuntimeError(f"{self.label}: another replica took over")
            self.last = time.time()

    def finish(self) -> None:
        self.curated.write(self.finished, self.lease())
