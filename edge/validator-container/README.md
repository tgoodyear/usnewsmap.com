# Validator container

The validation webhook for a [cloud-boinc](https://github.com/tgoodyear/cloud-boinc) server, run on Cloudflare: a Worker in front of a [Cloudflare Container](https://developers.cloudflare.com/containers/) that runs `usnm-boinc cloud validatord`. It validates the same apps as validatord does on a host (`usnm_gold`, `usnm_gold_native`, `usnm_ndlocr`, `usnm_quality`; see `crates/usnm-boinc/src/cloud/validatord.rs` and [bridge-steps.md](../../docs/boinc/server/bridge-steps.md)).

Dev only. Nothing here is deployed, and the buckets in `wrangler.jsonc` do not exist yet.

## What runs where

- **The Worker** (`worker.mjs`, with the decisions in `route.mjs`) answers every request first. Only `GET /healthz` and `POST /v1/validate/<app>` with the webhook's bearer token reach a container; anything else gets a 401, 404 or 405 without waking one. A forwarded request goes unchanged to one of `INSTANCES` container instances, picked at random (`getRandom` from `@cloudflare/containers`).
- **The container** (`Dockerfile`, `entrypoint.sh`) mirrors the gold directory from R2 with rclone, then starts validatord on port 8787. It sleeps after 15 minutes without a call (`sleepAfter` in `worker.mjs`) and starts again on the next one.
- **The state** (verdicts by call id, quality's seen copies and pair lines) is in R2 at `STATE_URL`, written create-only (`put_new`). Any instance can answer any call, a repeated call gets the stored verdict, and a container that is replaced loses nothing but its scratch space.

The gold check rejects a report for a unit it has no expectations for, so the gold mirror has to be complete before the unit's results come back. The entrypoint stops the container if the first sync fails and resyncs every five minutes after that. Gold units are submitted long before their results return, so the mirror stays ahead of them.

## Settings

Vars in `wrangler.jsonc`:

| Name              | Value                                                                       |
| ----------------- | --------------------------------------------------------------------------- |
| `CBOINC_BASE_URL` | The cloud server's root, to fetch outputs from.                             |
| `STATE_URL`       | `r2://<bucket>[/<prefix>]` for validatord's state.                          |
| `GOLD_URL`        | `r2://<bucket>[/<prefix>]` holding the gold directory `gold submit` writes. |
| `R2_ENDPOINT`     | `https://<account id>.r2.cloudflarestorage.com`.                            |

Secrets, set with `wrangler secret put <name>`:

| Name                                       | Value                                                                                                                |
| ------------------------------------------ | -------------------------------------------------------------------------------------------------------------------- |
| `WEBHOOK_TOKEN`                            | The bearer token the cloud server sends (its `WEBHOOK_TOKEN`). The Worker checks it, and so does validatord.         |
| `CBOINC_TOKEN`                             | An admin token of the cloud server, for reading outputs.                                                             |
| `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` | An R2 API token scoped to the state and gold buckets: read and write on the state bucket, read only on the gold one. |

The Worker hands all eight to the container as environment variables (`containerEnv` in `route.mjs`) and refuses to start one when any is missing. The entrypoint writes `WEBHOOK_TOKEN` to a file only the container's user can read and removes it from the environment before validatord starts.

## Tests

```sh
node --test
```

The tests cover `route.mjs`. validatord itself is tested in `crates/usnm-boinc` (`cargo test -p usnm-boinc --features cloud-validatord cloud::`), including two instances sharing one store.

## Deploying

Not done yet. When it is, from this directory, with Docker running (wrangler builds the image locally) and after the buckets and the token exist:

```sh
npm ci
npx wrangler secret put WEBHOOK_TOKEN   # and the other three
npx wrangler deploy
```

Then set each app's `validator` to `webhook` and its `validator_url` to `https://usnm-validator-dev.<account>.workers.dev/v1/validate/<app>` on the cloud server (see "Validators" in cloud-boinc's `cloud/docs/architecture.md`).
