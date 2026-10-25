# BOINC cross-host start guard

Since 21 October 2026 prod's bridge steps run on the on-prem host through
`scripts/boinc/onprem-step.sh`, which allows one step at a time with a host-local
lock rather than this lease ([boinc-onprem.md](boinc-onprem.md)). The guard below
covers the hand-off job, which stays in Azure, and the bridge job of a server with
`"server": "azure"`.

`scripts/boinc/start-boinc-bridge.sh` takes an infinite Azure blob lease around the
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
all nonterminal statuses, not just `Running` and `Processing`. Once visibility
is confirmed, the transaction lease is released before any `--wait-mins` poll
so nonconflicting deployments on another host can inspect the active execution,
while the next launcher or conflicting template update still sees the running
execution in the execution list.

## Azure call limits

`az` has no timeout of its own. On 20 October 2026 a status poll in the launcher waited more than
12 minutes for an execution that had already succeeded
([#932](https://github.com/tgoodyear/usnewsmap.com/issues/932)), and a call that hangs inside the
guard would hold the lease with nobody to end it. So the launcher and the helper bound their calls
on the job and the lease (`scripts/lib/bounded-az.sh`, written in shell because macOS has no
`timeout`). A call that runs out of time is killed together with the child processes Azure CLI
starts. The sign-in check and the stack-output lookups at the start of a run are not bounded.

- A read is killed after `BOINC_AZ_READ_TIMEOUT` seconds (60) and tried again, up to
  `BOINC_AZ_READ_ATTEMPTS` attempts (3) with `BOINC_AZ_READ_BACKOFF` seconds (5) between them. A read
  that fails is retried the same way. This covers the execution list, the status polls and
  read-backs, the template read, the exchange-account lookup and the blob existence check. Inside
  a window (the 120 seconds that confirm and reconcile allow, set by `BOINC_START_CONFIRM_SECS` and
  `BOINC_START_RECONCILE_SECS`, or the `--wait-mins` of a wait), an attempt is cut to what is left
  of the window and none starts after it ends. The first attempt of a call always gets at least 10
  seconds, so a window that is already over still reads once. A read that fails every attempt
  fails the way it did before: confirm and reconcile return failure, which keeps the lease, and a
  poll stops the run.
- A call that changes something is killed after `BOINC_AZ_WRITE_TIMEOUT` seconds (300) and not
  retried, because a request that timed out may have taken effect. These are the job update and
  start, the marker upload, and the lease acquire and release.
  - A timed-out job update or start is an uncertain outcome like any failed one: the run fails, and
    the reconcile described under Failure recovery decides whether the lease is released.
  - A timed-out lease acquire may have taken the lease on the server. The launcher stops before it
    touches the template, and the error names the blob and the proposed lease ID. If the blob is
    held under that ID, it was this launcher's, and releasing it with that ID is the clean way to
    free it; a release with any other ID fails, so trying it is safe.
  - A timed-out lease release reports that the release failed, with the blob and the lease ID, and
    the lease stays held.

These limits apply to `scripts/boinc/start-boinc-bridge.sh` and to the helper's use in
`scripts/boinc/provision-boinc.sh`. In production the controller still reaches the launcher for the
hand-off job's `export` and `texts-export` steps (its `claim`, `release` and `collect` run on the
on-prem host, [boinc-onprem.md](boinc-onprem.md#hand-off-steps-on-prem)), and for the bridge job of
a `"server": "azure"` environment. The other `baz` callers (the stack deployments, the ACR builds)
have no such timeout on purpose, since their calls run long.

## Coordination boundaries

This transaction lease is deliberately **not**
`boinc-handoff/scheduler/controller.lock`. The batch controller already holds
that infinite lease for its whole tick and invokes this launcher as a child;
recursively acquiring it would deadlock normal controller operations. Its
children automatically honor the separate per-job start lease without changing
the controller or passing lease credentials between processes.

Different jobs have separate leases; the guard does not forbid a bridge and
handoff execution from overlapping. It also cannot fence raw Azure CLI/API
writers or provisioning scripts that do not acquire this lease. Per
[#856](https://github.com/tgoodyear/usnewsmap.com/issues/856), production
deploys and job runs use **per-resource conflict checks** rather than a global
`all-lifecycle-idle` or `all-six-idle` gate. A running long-lived job (such as
`usnm-boinc-quality-caj-prod`) does not block app-version publication, a
server/CGI/daemon image rollout, or starts of a different job. The deploy
record (who deployed what, and when, from
[#529](https://github.com/tgoodyear/usnewsmap.com/issues/529)) is unchanged. A
start lease is not permission to deploy or interrupt another owner's work.

| Change type                                                                                             | Required gate                                                                                                                                                                                                                                                                                                   |
| ------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Add-only DB/project changes (publish app versions, create campaigns or workunits, plan-class additions) | [#529](https://github.com/tgoodyear/usnewsmap.com/issues/529) claim plus the controller lease; no idle requirement                                                                                                                                                                                              |
| Updating one job's template or image                                                                    | That job's per-job start lease (`scripts/lib/boinc-start-guard.sh`) and no nonterminal execution **of that job**. Other jobs keep running.                                                                                                                                                                      |
| Server/CGI/daemon image rollout, config or TLS changes                                                  | Claim plus controller lease. Must not replace or alter the template of any job with a running execution: defer just that job's template update (or let the execution finish naturally), not the whole rollout. Executions already running keep their template, so a stack update must not stop or restart them. |
| Full reprovision, DB migration or destructive operations                                                | All-idle allowed only as a documented extenuating circumstance, with the reason recorded in [#529](https://github.com/tgoodyear/usnewsmap.com/issues/529)                                                                                                                                                       |

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
