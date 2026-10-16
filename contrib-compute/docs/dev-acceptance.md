# Isolated BOINC dev acceptance

This is a proposed, executable acceptance path, **not deployment approval**.
Do not merge or provision during the API stable-head release hold. No resources,
roles or DNS records were changed while preparing this plan.

## Decision required

Approve a **24-hour, westus2, BOINC-only** window in subscription
`25bf257c-c94e-4d61-bba3-edc635f46602`, with an operator responsible for teardown.
The proposed spending allowance is **USD 20**, not an enforced billing limit.
Azure budgets and tags do not stop spending. If the preview or price refresh
exceeds that allowance, stop and obtain a new decision.

Approve these scopes explicitly:

- New `usnm-boinc-rg-dev`, its managed Container Apps infrastructure group, and
  resources listed below. Do not deploy `infra/main.bicep`, run
  `scripts/provision.sh --env dev`, change the existing application dev settings,
  or grant anything on a production queue, ledger, archive, storage account or
  registry.
- Existing subscription policy **definitions are referenced, not changed**.
  Seven assignments apply to the new dev group; only the existing BOINC Files
  shared-key and Log Analytics local-auth waivers apply there.
- Two new dev managed identities. Within dev: runtime gets Key Vault Secrets
  User, secrets identity gets Secrets Officer, both get AcrPull on the **new dev
  registry**. The explicitly selected operator gets Secrets Officer on the dev
  vault and Log Analytics Reader on the dev workspace. No new production grants.
- The unavoidable shared-authority exception is **two DNS records**, not a role
  assignment: `boinc-dev` CNAME and `asuid.boinc-dev` TXT in
  `usnewsmap.com`, group `rg-usnm-prod`. Existing operator authorization must
  suffice; if it does not, stop rather than broadening grants. The new optional
  dev-only `dns` setting separates this authority from the new registry group.
  Preview must not touch `boinc`, `asuid.boinc`, apex records or other names.
- Read/import only the two reviewed runtime manifests from the existing
  production ACR into the dedicated dev ACR. Preserve their manifest digests.
  No source-registry build, tag replacement, grant or production pin change.

No test VM, GPU, RHEL host, VNet peering, public data endpoint, signed production
app version, gold corpus download, handoff job or OCR-node installation is in
this initial scope. Only synthetic public inputs belong on the dev share.

## Resource and price scope

The existing BOINC template, without a speculative downsizing change, creates:

| Resource                      | Quantity / setting                                                                   | Billing scope                                                                                    |
| ----------------------------- | ------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------ |
| Private MySQL flexible server | 1, B1ms, MySQL 8.4, 20 GiB, 360 IOPS, HA off, 7-day LRS backups, auto-IO scaling off | Compute/storage; auto-grow is on and is **not** a cost cap                                       |
| Container Apps environment    | 1, Consumption profile, external ingress, private VNet integration                   | No dedicated profile or GPU; refresh applicable environment/network charges                      |
| Server                        | Exactly 1 replica, 1 vCPU / 2 GiB, never scales to zero                              | Budget as active, not idle                                                                       |
| Manual jobs                   | Secrets 0.25 vCPU / 0.5 GiB; init and bridge 0.5 vCPU / 1 GiB                        | No schedule; retries zero; max 600/1800 seconds per execution                                    |
| Azure Files                   | 1 Standard LRS account/share, transaction optimized, 200 GiB quota                   | Billed actual storage and operations, **not** 200 GiB preallocation; quota is not a spending cap |
| Private endpoints             | 2, Files and Key Vault                                                               | Endpoint hours and processed data                                                                |
| Private network               | 1 unpeered VNet, 3 subnets, 3 private DNS zones and 3 links                          | The default address range overlaps prod, but no routing/peering is authorized                    |
| Key Vault / identities        | 1 Standard vault, 2 identities                                                       | Vault operations; no production secrets copied                                                   |
| Logs                          | 1 workspace, 30-day retention, 1 GiB/day ingestion cap                               | Cap enforcement can lag; not a hard dollar ceiling                                               |
| Registry                      | 1 Basic ACR, admin disabled                                                          | Registry day/storage and import-related transfer                                                 |
| Frontend                      | 1 Free Static Web App                                                                | Not a premium plan                                                                               |
| Domain                        | Managed certificate + the two scoped parent-zone records                             | Existing DNS query/record charges                                                                |
| Other                         | No workbook in dev; goldBundle=false; appver empty                                   | No signing/publishing or automatic work submission                                               |

Retail API checks on 2026-10-16 for westus2/USD returned B1ms
`$0.017/hour` (meter `dfb90e38-ada8-5610-8e4a-38021c9f6bc8`),
MySQL storage `$0.115/GB-month`, Basic ACR `$0.1666/day`, ACA active
`$0.000034/vCPU-second` plus `$0.000004/GiB-second`, and Log Analytics
`$2.30/GB` after applicable free tiers. Thus 24 hours of the server at fully
active rates is `$3.6288`; B1ms is `$0.408`; 20 GiB MySQL storage prorated over
730 hours is about `$0.0756`; Basic ACR is `$0.1666`: **$4.279 known subtotal**,
not a total quote. One hour of bridge/init jobs adds `$0.0756`.

The same API returned Standard LRS transaction-optimized Files storage
`$0.06/GB-month`, reads `$0.0015/10,000`, writes `$0.015/10,000`, and Standard
Key Vault operations `$0.03/10,000`. Other SMB operation classes and excess
registry storage are additional. A Private Link meter could not be resolved
reliably from the API (empty results / HTTP 429); obtain that quote before
approval rather than substituting a remembered rate or treating it as free.

Private endpoint/data, SMB stored bytes/operations, DNS, Key Vault operations,
backup overage, bandwidth and applicable ACA environment charges are additional.
The API also lists `$0.10/environment-management-hour`; include `$2.40/day`
conservatively until its applicability to this Consumption environment is
confirmed. A full 1 GiB log cap adds `$2.30/day` before free tiers. No free tier,
reservation, subscription credit, Spot discount or tax exemption is assumed.
Do not describe the proposed $20 allowance as an exact Azure quote or hard cap.

Refresh the public retail meters before approval:

```sh
python3 - <<'PY'
import json, urllib.parse, urllib.request
query = "armRegionName eq 'westus2' and serviceName eq 'Azure Container Apps'"
url = "https://prices.azure.com/api/retail/prices?" + urllib.parse.urlencode({"$filter": query})
with urllib.request.urlopen(url, timeout=60) as response:
    prices = json.load(response)
for item in prices["Items"]:
    if item["type"] == "Consumption":
        print(item["meterName"], item["retailPrice"], item["unitOfMeasure"])
assert not prices.get("NextPageLink"), "Follow all pages before treating the quote as complete"
PY
```

Use the same API for MySQL, Container Registry, Log Analytics, Storage, Private
Link and Key Vault. A missing meter or HTTP 429 is not a zero-price quote.
The exact variable bill cannot be known without measured runtime usage.

## Offline preparation, allowed during the hold

Use the installed compatible Azure Bicep CLI (`az bicep version`; tested with
0.47.16). The older standalone 0.25 CLI cannot compile this repository's naming
functions.

```sh
mkdir -p target/boinc-dev-plan
export TMPDIR="$PWD/target/boinc-dev-plan"
az bicep build --file contrib-compute/infra/boinc/dev-bootstrap.bicep \
  --outfile target/boinc-dev-plan/bootstrap.json
az bicep build --file contrib-compute/infra/boinc/main.bicep \
  --outfile target/boinc-dev-plan/runtime.json
python3 -m unittest scripts/boinc/test_dev_acceptance.py
```

`dev-bootstrap.bicep` is subscription scoped but hardcodes the **dev** group and
dedicated dev registry names; it does not create or change policy definitions,
production resources, identities or role assignments. Its expiry tag is a
reminder, **not automatic deletion**.

## After approval and API slot release only

Run from a clean, reviewed checkout including this plan and #610, using current
operator scripts (including #619). These commands are **not authorized now**.

1. Review bootstrap What-If. Set the explicit teardown timestamp supplied by the
   operator, and verify the subscription again. An unexpected existing dev group
   requires inventory/ownership reconciliation, not an overwrite.

   ```sh
   set -euo pipefail
   SUB=25bf257c-c94e-4d61-bba3-edc635f46602
   : "${BOINC_DEV_EXPIRES_AT:?Set the approved UTC teardown deadline}"
   test "$(az account show --subscription "$SUB" --query id -o tsv)" = "$SUB"
   test "$(az group exists --subscription "$SUB" --name usnm-boinc-rg-dev)" = false
   az deployment sub what-if --subscription "$SUB" --location westus2 \
     --template-file contrib-compute/infra/boinc/dev-bootstrap.bicep \
     --parameters expiresAt="$BOINC_DEV_EXPIRES_AT"
   ```

   Apply only after the preview's seven policy assignments and two exemptions
   are approved; no resources outside the new dev group should be changed.

   ```sh
   az deployment sub create --subscription "$SUB" --location westus2 \
     --name boinc-dev-bootstrap \
     --template-file contrib-compute/infra/boinc/dev-bootstrap.bicep \
     --parameters expiresAt="$BOINC_DEV_EXPIRES_AT"
   DEV_REG=$(az deployment sub show --subscription "$SUB" --name boinc-dev-bootstrap \
     --query properties.outputs.registry.value -o tsv)
   ```

2. Generate isolated settings for review, with an explicitly selected operator.
   The generator makes **no Azure calls**, cannot overwrite deployment settings,
   rejects the production registry, and writes only below ignored `target/`.

   ```sh
   : "${BOINC_DEV_OPERATOR_ID:?Select the approved dev operator UUID}"
   : "${BOINC_DEV_OPERATOR_LOGIN:?Select the approved printable dev operator login}"
   python3 scripts/boinc/prepare_dev_acceptance.py \
     --registry-name "$DEV_REG" --operator-id "$BOINC_DEV_OPERATOR_ID" \
     --operator-login "$BOINC_DEV_OPERATOR_LOGIN" \
     --output-dir target/boinc-dev-inputs
   ```

   Review/commit `base-settings.json` as
   `contrib-compute/infra/boinc/env/dev.json`, and `names.json` as
   `infra/names/dev.json`, through a separate PR. Do **not** overwrite
   `infra/env/dev.json` or `infra/names/prod.json`. No production pins are changed.

3. Preview the base-only stack with existing tooling; it will create the
   private DB/vault/share/network but no app, jobs or DNS records yet.

   ```sh
   scripts/provision-boinc.sh --env dev --what-if
   # Only after independent preview approval:
   scripts/provision-boinc.sh --env dev
   ```

4. Import reviewed artifacts without changing production source tags:

   ```sh
   SERVER_DIGEST=sha256:d596fa1e26ce18ca1213297f16b3c692aca2481ceac572d19028340ade84ba54
   BRIDGE_DIGEST=sha256:3e89b6f349e45ec919e9276863838fc439b54a2fff6af0d0a6dea8b331bbff0d
   for item in "server:$SERVER_DIGEST" "bridge:$BRIDGE_DIGEST"; do
     kind=${item%%:*}
     digest=${item#*:}
     az acr import --subscription "$SUB" --name "$DEV_REG" \
       --source "crusnmprodffapzy.azurecr.io/usnm-boinc-$kind@$digest" \
       --image "usnm-boinc-$kind:reviewed-957413a9"
     test "$(az acr repository show --subscription "$SUB" --name "$DEV_REG" \
       --image "usnm-boinc-$kind:reviewed-957413a9" --query digest -o tsv)" = "$digest"
   done
   ```

   Promote the generated `runtime-settings.json` into the **dev** settings via
   review/commit. Its worker index is the separately published compatible v2
   image, not the current production worker pin. Appver remains empty and
   goldBundle false.

5. Preview app/jobs/DNS deployment with `scripts/provision-boinc.sh --env dev
--what-if`. Stop if anything alters production resources other than the two
   explicitly approved dev DNS names. After approval, run the same command
   without `--what-if`; it performs dev secrets, idempotent project init, app
   registration and actual public project-config health checks. It must not
   print ready if the hostname/certificate/DB initialization is unsuccessful.
   Keep preview IDs, job execution IDs and redacted logs.

   There is also a runtime-path preflight before this app phase: existing
   `supervisord.conf` writes an absolute system-temp PID and several bridge
   campaign steps use default-temp stamps. The current operator session forbids
   those paths. #610 fixes **image-build staging only**, not every runtime path.
   Prepare/review a dev-only runtime PID/stamp configuration, rebuild and
   revalidate the resulting dev manifest before starting those consumers under
   that constraint. Do not silently run the forbidden paths or claim #610 fixed
   runtime staging. The base-only preview/provisioning phase has no such
   container execution. Its cost/scope decision can therefore be made separately
   from the subsequent runtime-harness approval.

## Acceptance order and evidence

1. **Actual TLS first, still OFF.** Record the dev DB host, image digests, live
   `require_secure_transport=OFF`, and the exact rendered project paths. In the
   dev runtime, exercise actual pinned Python `Boinc.db_base`, both PHP database
   APIs, C++ `DB_CONN` consumers and the CLI admin/canonical-host calls. Record
   a nonempty session cipher and verified server identity for each; successful
   mock tests, CA parsing, a public HTTPS URL or a compiled image are not enough.
   `--init-project` and the public PHP endpoint cover useful real paths, but do
   not establish every consumer. Inventory running daemons and scheduler too.
2. **Negative matrix, dev only.** Repeat each real connector with wrong host,
   wrong/untrusted CA, absent TLS options and a deliberately no-TLS peer. Use
   an isolated test job or disposable copy, not the serving image/share. Missing
   DNS/TCP reachability is not proof of certificate rejection. Keep categorical
   outcomes only; never print DB passwords or full `config.xml`. An acceptance
   runner or test-peer resource not already in this inventory needs its own
   reviewed command/template before use. No negative case is marked passed by
   this plan.
3. **Persisted migration.** Initialize a separate synthetic project with the old
   runtime on a dev-only share/snapshot, inventory copied Python/PHP/binaries,
   rehearse the explicit replacement and rollback, then verify all web/scheduler
   and daemon paths after a restart. Record before/after hashes and session
   ciphers. Existing init skips completed projects: changing an image **does
   not** replace persistent copies automatically. Do not deploy #631 to prod as
   a substitute. Snapshot/storage for this rehearsal needs scoped approval.
4. **Dev enforcement only after 1–3.** A separately reviewed dev-only parameter
   override/acceptance step may then set secure transport ON and repeat the
   matrix. Existing template defaults remain OFF; it would reset an ad-hoc
   parameter change on later provisioning. No production ON change is in scope.
5. **Trust/assignment next.** Use synthetic gold and an explicit independently
   calibrated threshold, immutable worker index and compatible separately signed
   **dev** app version. No threshold is supplied here. Test canonical-host
   identity, targeted-v2 campaign nonce, later-failure revocation and automatic
   new-host issuance; no broad host discovery/production submission. Until
   calibration/automation is supplied, trust remains disabled.
6. **Optional RHEL profile separately.** First build stable 8.2.13 using
   `scripts/boinc/build-node-client.sh --help` and the reviewed
   [node guide](../../docs/boinc-ocr-node.md). A selected disposable RHEL host
   requires its own resource/cost/identity approval; the Ubuntu/Docker test VM
   is not evidence for the dedicated-account/rootless RHEL profile. Do not
   install anything on the live OCR nodes. Actual effective cgroups, SELinux,
   dev OCR/assessment collection and a comparable throughput baseline are
   prerequisites, not documents to mark complete.

This provides executable, independently previewable bootstrap/import/lifecycle
commands for a real dev environment, rather than requiring the full application
dev stack. It does not claim that the runtime-path preflight or every acceptance
harness, measurement or scientific calibration already exists. Each missing
negative-peer/migration/RHEL fixture is an explicit next implementation step,
not a reason to reuse production or invent a pass.

## Teardown is part of the approval

At the approved deadline, preserve redacted evidence, confirm no needed jobs
remain, and delete only the reviewed **dev** BOINC deployment stack/resources,
dev bootstrap group and its managed Container Apps infrastructure group. Review
stack deny-delete handling first; a bare group deletion may be blocked. Remove
only the dev CNAME/TXT records if still owned by this test. Do not delete the
parent zone, shared registry, subscription policies or existing application dev
stack. Verify resource absence and cost exports; stopping MySQL alone leaves
storage/endpoints/registry costs and is not teardown. Retain the expiry/owner and
an explicit teardown command review before provisioning.

The scoped teardown commands, after checking the stack's complete resource
inventory against the approved dev resources and two dev DNS record IDs, are:

```sh
set -euo pipefail
az stack group show --subscription "$SUB" --resource-group usnm-boinc-rg-dev \
  --name usnm-boinc-dev --query resources -o json
# Stop on any unexpected resource, unsupported deletion or out-of-sync error.
# Never use a stack error bypass, and never delete the production BOINC stack.
az stack group delete --subscription "$SUB" --resource-group usnm-boinc-rg-dev \
  --name usnm-boinc-dev --action-on-unmanage deleteResources \
  --resources-without-delete-support fail --yes &&
az group delete --subscription "$SUB" --name usnm-boinc-rg-dev --yes &&
test "$(az group exists --subscription "$SUB" --name usnm-boinc-rg-dev)" = false
```

Before deleting the stack, record its dev managed environment's
`infrastructureResourceGroup` and verify that group's `managedBy` points to the
dev environment. Verify its automatic removal afterwards; any retained group
needs a new owned-resource inspection before manual deletion. The subscription
deployment history may remain as evidence; it is not billable compute.
