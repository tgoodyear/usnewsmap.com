#!/usr/bin/env bash
# Point the API app (APP in resource group RG) at IMAGE, wait until the new
# revision is the ready one, then check the app's own /readyz.
set -euo pipefail
: "${APP:?}" "${RG:?}" "${IMAGE:?}"
az config set extension.use_dynamic_install=yes_without_prompt
az containerapp update -n "$APP" -g "$RG" --container-name api --image "$IMAGE" -o none

latest="" ready=""
for _ in $(seq 60); do
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
