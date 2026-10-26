# Optional BOINC alongside the OCR worker

**The direct queue worker is retired** (#966): local OCR runs through BOINC
only, and no node runs `usnm-ocr-worker.service`. This page is kept for the
client setup and the retirement steps below; see the [BOINC pages](boinc/README.md)
for the current path.

Issue #472 is **not production acceptance**. Do not run provisioning, builds, or pilot work on production
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
detection. The script checks the revision, builds only client and library targets,
checks the resulting version and writes binary checksums. It never installs
host files or starts a service. Run it in a compatible **dev or build
environment**, not beside the worker; build prerequisites include git, GCC C++,
make, autoconf/automake/libtool, pkg-config, curl and OpenSSL development
packages. This RHEL source-build path still needs a real dev build and ABI test.

The existing volunteer helper intentionally does not create accounts or guess
subordinate ranges. Before using it, provision an empty, disabled and stopped
`boinc-client.service` running the verified stock binary directly as **boinc**
with its existing working directory owned by that account (normally
`/var/lib/boinc`). `boinc` must have its own UID/GID and non-overlapping blocks
of at least 65,536 subordinate UIDs **and** GIDs, distinct from `usnm` and
`usnmeng`. Select ranges from the node's actual allocation inventory; never
reuse the engine account's ranges, API socket, groups or image storage.
Account-database memberships **and** systemd `Group`/`SupplementaryGroups`
for both the BOINC client and user manager are checked. Worker and runtime
groups, `docker`, privilege groups and Arc's `himds` group are forbidden;
`himds` would allow access to the node's managed-identity challenge. Startup
repeats these checks and checks its actual process groups before any probe or
work. Review the service definition and package in dev before installation. Do
not replace or restart the OCR worker unit or modify its Podman configuration.

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

This reuses the tested preflight for rootless Podman, SELinux, slots and pause,
the separate runtime configuration and the existing subordinate-ID overlap
checks. Unlike the normal volunteer setup, OCR-node mode **does not start or
enable BOINC**. It enables and starts only the dedicated account's empty systemd
user session.

The entire `user-<boinc UID>.slice` gets a hard quota of **25% of one CPU**,
2 GiB memory, no swap and 256 processes, with CPU and IO weight 1. Both the
client service and rootless user manager must be in that slice: limiting only
the client service misses containers created under `user@UID.service`. The
client also gets nice 19 and idle IO. Existing BOINC preferences still allow
only one CPU, no GPUs, idle-only processing and no work buffer. These are
deliberately small **dev starting limits**, not a measured production budget.

Every startup reads the actual aggregate slice's `cpu.max`, `memory.max`,
`memory.swap.max` and `pids.max`, and refuses missing or weakened limits, or a
client outside that slice, **before it pulls, probes or runs tasks**. The
ordinary per-container limits and pause and slot checks still run.

## Dev acceptance and production blockers

1. Record equal-duration, comparable cohort pages/hour, worker RSS, CPU and
   IO, disk free space and queue depth with BOINC **off**. Save cohort IDs,
   times, worker and image versions, node resources, raw observations and the
   agreed non-disruption criterion before the experiment; do not fabricate a
   tolerance afterwards.
2. Use the dev BOINC server (ADR-0019), reserve a tiny explicit cohort for this
   host, start the optional client manually, and attach only to that dev project.
   Do not enable public production assignment.
3. Verify kernel aggregate quotas for both BOINC and its actual container
   cgroups, rootless ownership and storage, SELinux enforcing, and suspend and
   resume.
   Verify OCR upload, validation **and collection**, then a small `usnm_assess`
   campaign. Do not mistake exit zero or scheduler reporting for acceptance.
4. Repeat the same throughput measurement with BOINC on. Stop BOINC immediately
   if worker throughput, latency or resource pressure violates the pre-agreed
   rule. Account for cache warming, the source and language mix and background
   updates.
5. Rehearse rollback: suspend, allow no new work, drain if safe, then
   `sudo systemctl disable --now boinc-client.service`. Check no BOINC task
   containers remain under the dedicated account; stop only that account's
   runtime if necessary, never `usnmeng`. Recheck worker throughput. Keep
   accounts, data and backups for inspection; do not prune shared storage.

No production provisioning, restart or deployment was performed for this change.
These are still blocked: acceptance of the dev client build, install, service
and account provisioning; real verification of the aggregate cgroup and SELinux;
results from the dev server, cohort and assessment; and measured
before-and-after throughput. Automated node account provisioning and production
enablement are deliberately deferred until this plan is validated.

## Retire an OCR node to BOINC only

Sharing a node measurably slowed the queue worker: on node 4 one capped gold
task dropped it from 44 to 28 pages/hour. Every node has since retired its
worker (#966). On a node that still has the unit, retire it and switch the
dedicated UID to the dedicated-node profile:

```sh
# 1. Drain and retire the queue worker (in-flight pages get their grace period;
#    anything abandoned reappears on the queue). The reboot window only
#    restarts what it stopped.
sudo systemctl disable --now usnm-ocr-worker.service
# 2. Let queued BOINC work finish and report (No new tasks), then:
python3 scripts/boinc/setup_volunteer.py --service usnm-boinc-canary.service --dedicated-node
sudo python3 scripts/boinc/setup_volunteer.py --service usnm-boinc-canary.service --dedicated-node --apply
```

`--dedicated-node` refuses to run while `usnm-ocr-worker.service` is enabled
or running, keeps the same identity and group checks as `--ocr-node`, and
replaces the 25%-of-one-CPU slice file with `zz-usnm-dedicated-node.conf`: no
CPU quota, 85% of memory, no swap, 1024 tasks, lowest CPU and IO weight, nice 19
and idle IO.
BOINC gets every CPU (`--ncpus N` to leave some free), runs while someone is
logged in, may use 85% of memory (the slice cap; on an 8 GB node this admits
the 6 GiB qwen units, which 80% did not), and is enabled at boot. Every start's
preflight refuses an enabled or running OCR worker and a slice without its
memory, swap or task limits. Accounts and attachments are kept.

There is no way back to the direct worker: its unit and installers are removed,
and local OCR runs through BOINC only.
