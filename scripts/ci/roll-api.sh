#!/usr/bin/env bash
# Point the API app (APP in resource group RG) at IMAGE, wait until the new
# revision is the ready one, then check the app's own /readyz.
set -euo pipefail
: "${APP:?}" "${RG:?}" "${IMAGE:?}"
az config set extension.use_dynamic_install=yes_without_prompt

# The searcher sidecar's config is infra/quickwit/searcher.yaml. Apply it
# when the running one differs, so a change to it goes live on merge rather
# than waiting for scripts/provision.sh. The storage account is read from
# the running config (the file has a placeholder for it). An app without
# the sidecar (the memory backend) is left alone.
current=$(az containerapp show -n "$APP" -g "$RG" \
  --query "properties.template.containers[?name=='quickwit'] | [0].env[?name=='USNM_QW_CONFIG'] | [0].value" \
  -o json | jq -r '. // empty')
if [ -n "$current" ]; then
  account=$(sed -n 's/^ *account: *//p' <<< "$current" | head -n 1)
  [[ $account =~ ^[a-z0-9]{3,24}$ ]] ||
    { echo "::error::no storage account in the running sidecar config"; exit 1; }
  wanted=$(sed "s/__STORAGE_ACCOUNT__/$account/" infra/quickwit/searcher.yaml)
  # az reads "secretref:" in a value as a reference to a secret.
  if grep -q 'secretref:' <<< "$wanted"; then
    echo "::error::infra/quickwit/searcher.yaml must not contain secretref:"; exit 1
  fi
  if [ "$wanted" != "$current" ]; then
    az containerapp update -n "$APP" -g "$RG" --container-name quickwit \
      --set-env-vars "USNM_QW_CONFIG=$wanted" -o none
    echo "applied infra/quickwit/searcher.yaml to the searcher sidecar"
  else
    echo "the searcher sidecar's config is current"
  fi
fi

az containerapp update -n "$APP" -g "$RG" --container-name api --image "$IMAGE" -o none

# The update returns once the new revision is created, not when it's ready.
# In single-revision mode the old revision serves until the new one passes
# its startup and readiness probes, and with min replicas above 0 the API
# stays not ready through its startup warm-up: loading plus up to 360 s,
# within the startup probe's 780 s (infra/modules/containerapp.bicep). Wait
# up to 15 minutes.
latest="" ready=""
for _ in $(seq 90); do
  revs=$(az containerapp show -n "$APP" -g "$RG" \
    --query "[properties.latestRevisionName, properties.latestReadyRevisionName]" -o tsv)
  latest=$(sed -n 1p <<< "$revs")
  ready=$(sed -n 2p <<< "$revs")
  if [ -n "$latest" ] && [ "$latest" = "$ready" ]; then
    echo "revision $latest is ready"
    break
  fi
  sleep 10
done
test "$latest" = "$ready" || { echo "::error::revision $latest did not become ready"; exit 1; }

# Always check the app's own address; no variable can skip this.
fqdn=$(az containerapp show -n "$APP" -g "$RG" --query properties.configuration.ingress.fqdn -o tsv)
test -n "$fqdn" || { echo "::error::$APP has no ingress FQDN"; exit 1; }
curl -fsS --retry 10 --retry-delay 6 --retry-all-errors "https://$fqdn/readyz"
