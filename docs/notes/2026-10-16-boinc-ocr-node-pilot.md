# BOINC beside the OCR worker on the nodes

**Status:** superseded: the direct queue worker is retired (#966), and no node runs `usnm-ocr-worker.service` · **Date:** 16 October 2026 · **Measured on:** node 4's worker before any BOINC install (16 October 2026), and one capped gold task beside it · **Code:** `boinc/scripts/setup_volunteer.py` (`--ocr-node`, `--dedicated-node`), `build-node-client.sh`, `build-node-client-container.sh`, `observe_node.py`, `prepare_node.py`, `client_account.py` · **Issues:** #472, #966

The plan for running a capped BOINC client on an OCR node beside its queue worker, the tools written for it, and how a node was retired to BOINC only. Moved here from the BOINC server pages on 10 November 2026; the current path is in the [BOINC pages](../boinc/README.md).

Issue #472 is **not production acceptance**. Do not run provisioning, builds, or pilot work on production
until an independently reviewed dev plan and comparable pages/hour baseline
show spare CPU, RAM, IO and disk capacity. A hard cap limits damage; it cannot
prove zero throughput impact.

## Client source and dedicated identity

The official [Linux selector](https://boinc.berkeley.edu/linux_install.php)
currently offers stable **8.2.13**, but does **not** list RHEL. Do not substitute
an old EPEL client or enable an unsupported Fedora repository on RHEL 10.
`boinc/scripts/build-node-client.sh --out /absolute/new/build-dir` builds the
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

Install the [volunteer runtime prerequisites](../boinc/volunteer/README.md#install-prerequisites).
The BOINC client and user session must be inactive, with no queued work, and
the BOINC service disabled. There is no auto-attach and no account key in this
setup.

## Preview and install the optional profile

```sh
python3 boinc/scripts/setup_volunteer.py --ocr-node
# Only in the approved dev installation, after reviewing the preview:
sudo python3 boinc/scripts/setup_volunteer.py --ocr-node --apply
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
python3 boinc/scripts/setup_volunteer.py --service usnm-boinc-canary.service --dedicated-node
sudo python3 boinc/scripts/setup_volunteer.py --service usnm-boinc-canary.service --dedicated-node --apply
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

## Node readiness snapshots

Build the actual pinned stable client away from a production OCR node:

```sh
boinc/scripts/build-node-client-container.sh --out "$PWD/target/boinc-node-client"
```

The pinned AlmaLinux 9 builder uses the existing `build-node-client.sh`
(`ae68f3290ac82d08b4d9767acfe0daaccaeeb883`, stable 8.2.13), one make job, and an
amd64 compilation container capped at one CPU, 1.5 GiB memory with no extra swap,
and 256 tasks. The dependency-image build is separate; a Docker BuildKit
`build --memory` flag is not relied upon. The compiler container checks actual
cgroup limits and exports those readings alongside binaries, source commit,
dependency image ID and relative SHA256SUMS; it never installs on a node. Actual
configure requires `zlib-devel` as well as curl and OpenSSL development packages.
CI builds the real client and retains a tar archive for seven days, preserving
executable modes. It re-extracts that archive, verifies executable bits,
checksums and actual cgroup limits, and runs both loaders plus `boinc --version`
inside pinned EL10 with no network. This native CI check passed for stable
8.2.13; local ARM emulation instead fails at EL10 glibc's x86-64-v3 requirement
before BOINC starts. An exported bundle still requires actual verification of the
RHEL host's ABI and version before installation; successful CI is not host or
client acceptance.

Take a genuine read-only worker snapshot **before** installing or starting a
canary, and again during and after any separately approved bounded cohort:

```sh
python3 boinc/scripts/observe_node.py \
  --host <user>@<node-host> \
  --window-seconds 900 --out target/node4-before.json
```

The identical checked-in observer runs through SSH stdin under read-only sudo
commands, installs no host files and never starts or stops a service. It counts
actual successful `page done` events in the complete window, not an extrapolated
gap between selected page samples. It reports pages/hour, median page duration,
last completion age, errors, engine, language and queue composition, unchanged
worker PID and start time, CPU and memory cgroup counters, host memory and load,
and available pressure metrics. No page text, credentials or account keys are
returned. Output refuses to overwrite an existing evidence file.

`baseline_ready` requires an active worker, at least ten successful pages in the
window, no errors and a completion within five minutes. These are conservative
**pre-pilot readiness** checks, not calibrated regression or throughput
thresholds. They must not be used to claim that two windows have equivalent
workloads or that an after-window passed a non-disruption contract. Choose the
bounded cohort, observation windows and rollback rule before starting; preserve
worker PID, queue and engine mix and actual resource deltas in the evidence.

An actual Node 4 pre-pilot snapshot at 2026-10-16 11:22 UTC had one successful
page in 900 seconds (4/hour), last completion 891 seconds earlier, while three
existing Tesseract containers each consumed about 96% CPU for 14–15 minutes. The
production worker was still active, unchanged since October 14, with no OOM
events. No BOINC account, subordinate IDs, client or service was installed;
adding pilot load was withheld. An active process alone is not a comparable
healthy throughput baseline. Let the maintainer resolve the existing workload or
obtain a naturally valid fresh baseline rather than restarting or replacing the
worker, attributing the existing slowdown to BOINC, or purchasing dev resources.
At 11:44:50 UTC it naturally recovered: 23 successful pages in 900 seconds
(92/hour), median 115.50 seconds, zero failure events and last completion 45.8
seconds earlier, with the same worker PID and start time. This meets readiness,
not the no-regression acceptance of the still-unexecuted during-pilot and
after-pilot windows.

## Disabled client prerequisites

`boinc/scripts/prepare_node.py` is a separate explicit opt-in for the user-approved
existing-node pilot. It does not install packages, adopt existing accounts, start
or enable a service, create a project attachment, or modify worker or timer units.
It refuses an existing `boinc` account, home, install or unit, verifies both
exported binary hashes and the stable source commit, requires a ready baseline no
more than 120 seconds old with the same live worker PID, then previews the exact
plan:

```sh
sudo python3 boinc/scripts/prepare_node.py \
  --bundle /path/to/extracted/boinc-node-client \
  --baseline /path/to/fresh-node4-before.json
```

Only an explicit `--apply` creates the locked, nologin `boinc` identity, fresh
non-overlapping 65536-entry subuid/subgid ranges, owned home, client binaries,
and disabled `usnm-boinc-canary.service`. It executes `--version` as that actual
unprivileged host UID, then verifies the worker PID stayed unchanged. The base
unit cannot start until the reviewed volunteer-profile drop-in exists. Apply
`setup_volunteer.py --ocr-node --service usnm-boinc-canary.service` separately
to configure and verify whole-UID limits, SELinux and rootless runtime; that
helper also leaves the client stopped and disabled. Neither step authorizes an
attachment by itself. Production attachment requires explicit account and master
authorization, a fresh healthy baseline and the verified capped profile; do not
infer consent or enable unreviewed apps or GPU work.

Rollback an explicitly started pilot with:

```sh
sudo python3 boinc/scripts/prepare_node.py --stop
```

This stops and disables only the managed canary client and its dedicated user
manager, disables lingering, and retains the account, files and task evidence
for inspection. It does not delete subordinate-ID allocations or kill or
restart OCR. If preparation fails before the unit is written, no service was
started; inspect the retained identity and files instead of retrying over an
existing account. The source and tests alone are not evidence that this host
setup or a returned cohort has executed.

## Private managed-client account attachment

`boinc/scripts/client_account.py` uses the stock BOINC GUI-RPC protocol on loopback.
Preview with `--data-dir`, `--port`, `--expected-user-id`, and
`--expected-user-name`; it prints only whitelisted attribution and status fields,
not the account name, authenticators, or raw RPC replies. Explicit
`--apply --key-file` accepts only a regular, nonsymlink, private weak-key file
matching the expected user ID. Never pass the key itself as an argument. The
canonical production URL is `https://boinc.usnewsmap.com/`; legacy or duplicate
attachments require reconciliation instead of silently creating a second project.

Credential-bearing debug flags are checked against the latest retained **effective
runtime** core log-flag dump as well as the on-disk configuration; missing or
potentially truncated runtime evidence refuses attachment. Stock startup and
supported config reread emit this dump. Project messages cannot substitute for
core runtime evidence. The versioned private drain journal is bound to this
machine, real data path, RPC port, and GUI-auth-file identity; legacy or
cross-client journals are refused. A successful late-attribution retry releases
only the validated helper-owned hold and removes its journal without detaching
again. Network policy is restored after the **last** attach or update RPC,
including initially unattached clients. Every apply holds a nonblocking exclusive
lock in the real client data directory from before RPC authentication through
final network restoration and journal cleanup. The lock is independent of key and
journal paths; its inode is never removed. Concurrent operators must retry instead
of racing detach against a new attachment.

Stock BOINC 8.2 GUI-RPC authentication requires `MD5(nonce + GUI password)`.
This is a transient, loopback-only protocol challenge, not a stored password
hash. CodeQL's password-hashing warning requires an explicit audited decision
on this mandatory interoperability line; inline LGTM comments are not honored
by GitHub code scanning. This does not claim MD5 is suitable for password storage
or permit remote, plaintext GUI-RPC exposure.

The supported client rejects attaching an already attached URL. For an explicitly
authorized managed-client rejoin, the helper records the original work-enabled
preference in a private durable journal before requesting no new tasks. It refuses
to detach while any project result or scheduler RPC remains; reinvoke after
draining. A temporary client-wide network guard closes the final detach race and
restores the previous permanent or temporary network mode even on errors. Other
projects' transfers may pause briefly during this guard; their work is not reset.
The supplied key is used only in the private RPC payload. Attribution waits a
bounded time for the actual scheduler's expected user ID and name and nonzero host
ID.

Stock `project_attach` re-enables networking and cannot atomically preserve a
pre-existing user no-new-tasks hold. Therefore user-held and account-manager
projects are refused **without changing them**, rather than claiming a safe hold
that the client cannot provide. A helper-owned drain hold is distinguished by
its journal and resumes the original work-enabled policy after rejoining.
An independent GUI project suspension is also refused, including on reinvocation;
`disallow_attach` is checked before journaling or any project mutation so a valid
attachment is never detached from a client that prohibits reattachment.

Before attachment, capture effective global resource preferences and protect them
using the supported local-override RPC, preventing a new account's scheduler
reply from silently changing CPU, memory and other limits. The original override
and protected effective preferences are backed up privately beside the journal;
the explicit local resource policy remains installed and should be inspected
before subsequently changing account-wide preferences. Weekly CPU and network
schedules are materialized for all seven days so a later scheduler reply cannot
introduce an inherited day outside the protected limits. Unrestricted equal
endpoints become explicit 00:00–24:00 intervals; 24:00–00:00 remains never.
Stored and working schedules are compared semantically and must retain every
explicit day. Schedules that cannot round-trip the pinned client's two-decimal
weekly serialization are refused rather than silently rounded. For the stock
client's idle CPU-count percentage only, working value zero and parsed override
value 100 are equivalent (both leave the CPU count unlimited). Readback accepts
that documented representation change, not arbitrary limit changes. Weak-key
shape checks reject both 31- and 33-hex suffixes; valid shape alone cannot detect
a transcribed but different credential. Historical server records are not
rewritten. A standard detach can remove unused project downloads; inventory
caches and obtain explicit rejoin authorization first. This does not provision
resources, activate the optional OCR-node profile, grant GPU consent, or change
server pins or TLS policy.

For a dedicated, otherwise empty OCR-node client whose throughput comparison
remains uncertain, preserve its original `boinccmd --get_cc_status` first. Use
the installed supported `boinccmd --set_run_mode never` and
`--set_gpu_mode never` **before** attachment, using its data directory and
loopback RPC port (no credential arguments). After verified attachment,
immediately request project `nomorework` and `suspend`. The client stays
responsive and credit attribution can be checked without executing any new
BOINC tasks. Stock attach cannot atomically prevent an initial assignment;
report and preserve any queued results rather than claiming none were assigned.
Resume only this owned project and restore the recorded computation modes when
the bounded workload, CPU and memory comparison permits real work. Do not apply
this empty-client operational hold blindly to a client running other projects.
