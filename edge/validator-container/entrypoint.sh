#!/bin/sh
# Starts validatord in the container (README.md). The Worker passes every value below as an
# environment variable (containerEnv in route.mjs). Nothing here prints a token.
set -eu

for v in WEBHOOK_TOKEN CBOINC_TOKEN CBOINC_BASE_URL STATE_URL GOLD_URL \
  R2_ENDPOINT R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY; do
  eval "val=\${$v:-}"
  if [ -z "$val" ]; then
    echo "entrypoint: $v is not set" >&2
    exit 2
  fi
done

case "$GOLD_URL" in
  r2://?*) gold_remote="r2:${GOLD_URL#r2://}" ;;
  *)
    echo "entrypoint: GOLD_URL must be r2://<bucket>[/<prefix>]" >&2
    exit 2
    ;;
esac

# rclone's remote "r2", from the same credentials validatord uses.
export RCLONE_CONFIG_R2_TYPE=s3
export RCLONE_CONFIG_R2_PROVIDER=Cloudflare
export RCLONE_CONFIG_R2_ENDPOINT="$R2_ENDPOINT"
export RCLONE_CONFIG_R2_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID"
export RCLONE_CONFIG_R2_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY"
export RCLONE_CONFIG_R2_NO_CHECK_BUCKET=true

umask 077
home=/srv/validator
mkdir -p "$home/run" "$home/gold" "$home/work"
printf '%s' "$WEBHOOK_TOKEN" >"$home/run/webhook-token"
unset WEBHOOK_TOKEN

# The gold check rejects a report for a unit it has no expectations for, so the mirror must be
# complete before the first call: a failed first sync stops the container.
rclone sync --checksum "$gold_remote" "$home/gold"

# Gold units are submitted long before their results come back; a resync every five minutes keeps
# the mirror ahead of them. A failed resync keeps the last good mirror and is retried.
(
  while sleep 300; do
    rclone sync --checksum "$gold_remote" "$home/gold" ||
      echo "entrypoint: gold resync failed; keeping the last mirror" >&2
  done
) &

exec /usr/local/bin/usnm-boinc cloud validatord \
  --listen 0.0.0.0:8787 \
  --allow-non-loopback \
  --secret-file "$home/run/webhook-token" \
  --state-url "$STATE_URL" \
  --work-dir "$home/work" \
  --gold-dir "$home/gold" \
  --base-url "$CBOINC_BASE_URL"
