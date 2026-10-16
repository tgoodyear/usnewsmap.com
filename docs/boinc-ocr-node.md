# Optional BOINC alongside the OCR worker

Issue #472 is **not production acceptance**. The queue worker remains the
production path. Do not run provisioning, builds, or pilot work on production
until an independently reviewed dev plan and comparable pages/hour baseline
show spare CPU, RAM, IO and disk capacity. A hard cap limits damage; it cannot
prove zero throughput impact.

## Client source and dedicated identity

The official [Linux selector](https://boinc.berkeley.edu/linux_install.php)
currently offers stable **8.2.13**, but does **not** list RHEL. Do not substitute
an old EPEL client or enable an unsupported Fedora repository on RHEL 10.
`scripts/boinc/build-node-client.sh --out /absolute/new/build-dir` builds the
actual upstream `client_release/8.2/8.2.13` peeled commit
`ae68f3290ac82d08b4d9767acfe0daaccaeeb883`. That source contains direct Podman
detection. The script checks the revision, builds only client/library targets,
checks the resulting version and writes binary checksums. It never installs
host files or starts a service. Run it in a compatible **dev/build environment**,
not beside the worker; build prerequisites include git, GCC C++, make,
autoconf/automake/libtool, pkg-config, curl and OpenSSL development packages.
This RHEL source-build path still needs a real dev build/ABI test.

The existing volunteer helper intentionally does not create accounts or guess
subordinate ranges. Before using it, provision an empty, disabled/stopped
`boinc-client.service` running the verified stock binary directly as **boinc**
with its existing working directory owned by that account (normally
`/var/lib/boinc`). `boinc` must have its own UID/GID and non-overlapping blocks
of at least 65,536 subordinate UIDs **and** GIDs, distinct from `usnm` and
`usnmeng`. Select ranges from the node's actual allocation inventory; never
reuse the engine account's ranges, API socket, groups or image storage.
Review the service definition/package in dev before installation; do not
replace/restart the OCR worker unit or modify its Podman configuration.

Install the [volunteer runtime prerequisites](boinc-volunteer.md#install-prerequisites).
The BOINC client and user session must be inactive, with no queued work, and
the BOINC service disabled. There is no auto-attach and no account key in this
setup.

## Preview and install the optional profile

```sh
python3 scripts/boinc/setup_volunteer.py --ocr-node
# Only in the approved dev installation, after reviewing the preview:
sudo python3 scripts/boinc/setup_volunteer.py --ocr-node --apply
```

This reuses the tested rootless Podman/SELinux/slot/pause preflight, separate
runtime configuration and existing subordinate-ID overlap checks. Unlike the
normal volunteer setup, OCR-node mode **does not start or enable BOINC**.
It enables/starts only the dedicated account's empty systemd user session.

The entire `user-<boinc UID>.slice` gets a hard quota of **25% of one CPU**,
2 GiB memory, no swap and 256 processes, with CPU/IO weight 1. Both the client
service and rootless user manager must be in that slice: limiting only the
client service misses containers created under `user@UID.service`. The client
also gets nice 19 and idle IO. Existing BOINC preferences still allow only one
CPU, no GPUs, idle-only processing and no work buffer. These are deliberately
small **dev starting limits**, not a measured production budget.

Every startup reads the actual aggregate slice's `cpu.max`, `memory.max`,
`memory.swap.max` and `pids.max` and refuses missing/weakened limits or a client
outside that slice **before pulling/probing/running tasks**. The ordinary
per-container limits and pause/slot checks still run.

## Dev acceptance and production blockers

1. Record equal-duration, comparable cohort pages/hour, worker RSS/CPU/IO,
   disk free space and queue depth with BOINC **off**. Save cohort IDs, times,
   worker/image versions, node resources, raw observations and the agreed
   non-disruption criterion before the experiment; no fabricated tolerance.
2. Use the dev BOINC server (ADR-0019), reserve a tiny explicit cohort for this
   host, start the optional client manually, and attach only to that dev project.
   Do not enable public production assignment or replace the queue worker.
3. Verify kernel aggregate quotas for both BOINC and its actual container
   cgroups, rootless ownership/storage, SELinux enforcing and suspend/resume.
   Verify OCR upload, validation **and collection**, then a small `usnm_assess`
   campaign. Do not mistake exit zero or scheduler reporting for acceptance.
4. Repeat the same throughput measurement with BOINC on. Stop BOINC immediately
   if worker throughput/latency/resource pressure violates the pre-agreed rule.
   Account for cache warming, source/language mix and background updates.
5. Rehearse rollback: suspend/no-new-work, drain if safe, then
   `sudo systemctl disable --now boinc-client.service`. Check no BOINC task
   containers remain under the dedicated account; stop only that account's
   runtime if necessary, never `usnmeng`. Recheck worker throughput. Keep
   accounts/data and backups for inspection; do not prune shared storage.

No production provisioning/restart/deployment was performed for this change.
Still blocked: dev client build/install/service/account provisioning acceptance,
real aggregate-cgroup/SELinux verification, dev server/cohort/assessment results,
and measured before/after throughput. Automated node account provisioning and
production enablement are deliberately deferred until this plan is validated.
