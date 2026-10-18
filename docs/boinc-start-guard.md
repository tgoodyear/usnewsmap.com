# BOINC cross-host start guard

`scripts/start-boinc-bridge.sh` takes an infinite Azure blob lease around the
execution check, shared-template update, start and settings read-back. Both
`--target bridge` and `--target handoff` use the helper in
`scripts/lib/boinc-start-guard.sh`. A second cooperating launcher for the
**same job**, even on another host, fails before updating its template.
The bridge's existing host-local mutex is retained for compatibility with
provision/release child processes.

The existing main stack `usnm-<env>` supplies `OCR_EXCHANGE_ACCOUNT`. Leases
live in its existing `boinc-handoff` container, under
`scheduler/starts/<lowercase-resource-group>/<lowercase-job>.lock`.
The marker is created without overwrite; simultaneous first callers still
arbitrate through the atomic lease. Every Azure call names the subscription;
storage uses the operator's Entra login, not account keys or SAS tokens.
Missing outputs, denied storage access and unavailable leases fail closed.
This requires the exchange account to exist, including for a standalone BOINC
provision that calls the launcher. Do not bypass the guard, open the firewall,
or broaden permissions to work around a missing prerequisite. `--what-if`
remains read-only and does not acquire or create a lease.

After starting, the helper keeps its lease until settings match and the new
execution is visible as nonterminal in the execution list, or has already
ended. It waits up to 120 seconds for visibility. The execution check rejects
all nonterminal statuses, not just `Running` and `Processing`. With
`--wait-mins`, the lease remains held while the launcher waits. Without it,
the next caller's execution check rejects the still-active run.

## Coordination boundaries

This transaction lease is deliberately **not**
`boinc-handoff/scheduler/controller.lock`. The batch controller already holds
that infinite lease for its whole tick and invokes this launcher as a child;
recursively acquiring it would deadlock normal controller operations. Its
children automatically honor the separate per-job start lease without changing
the controller or passing lease credentials between processes.

Different jobs have separate leases; the guard does not forbid a bridge and
handoff execution from overlapping. It also cannot fence raw Azure CLI/API
writers or provisioning scripts that do not acquire this lease. Production
provisioning, template changes and multi-step runs still require the operator
claim, all-lifecycle-idle checks and controller lease documented in
[#529](https://github.com/tgoodyear/usnewsmap.com/issues/529). A start lease is
not permission to deploy or interrupt another owner's work.

## Failure recovery

Normal exits release the transaction lease and private local staging directory.
An uncertain template update or start response, mismatched/unreadable execution settings, or failed
visibility check deliberately **retains the infinite lease**, even though the
host-local mutex is released. A crashed process or failed lease release can
also leave it held. There is no expiry, automatic takeover or forced stop.

Before explicitly breaking that one blob lease, coordinate in #529, confirm the
former launcher is stopped, inspect the job's full execution list and settings,
and reconcile every possibly started execution and in-flight template mutation. Preserve live jobs and their
journals. Only after the outcome is known and the prior owner cannot write may
the operator break the stale lease using the normal Azure blob lease tooling.
Do not delete marker blobs, break a live lease, or retry blindly. The next
launcher still checks for any active execution before writing the template.
A release failure is a launcher failure, not silently reported success.
