# The bridge steps

The bridge runs one step of `boinc/images/server/bridge-job.sh` per run, chosen by `BRIDGE_STEP` and that step's settings: submission (`submit`, `submit-remote`, `reconcile`), the assimilator spool (`spool-out`), the controller's `batch-inventory`, app registration, and the staging, submission and collection steps of each campaign app (`gold-*`, `quality-*`, `qwen-*`, and the retired `surya-*`, `iris-*`, `assess-*` and `train-*`).

## Start a bridge step

On the on-prem BOINC host the step runner `boinc/scripts/onprem-step.sh` is installed as `usnm-boinc-step`:

```sh
usnm-boinc-step --env prod --target bridge --step <step> [--var NAME=VALUE ...] --wait-mins N
```

`--wait-mins` is required. The step runs in a container from the bridge image named by `SERVER_IMAGE` in `/etc/usnm-boinc/server.env`, with the settings in `/etc/usnm-boinc/bridge.env` plus `BRIDGE_STEP` and each `--var`. One step runs at a time: a second caller fails while a step holds `/run/usnm-boinc-step.lock`, unless the host sets `USNM_BOINC_STEP_LOCK_WAIT_SECS` (0 to 300, taken from the `--wait-mins` budget). `quality-stage` has a lock of its own, and the hand-off steps, `spool-out` and `batch-inventory` share `/run/usnm-boinc-handoff.lock` ([hand-off steps on-prem](onprem.md#hand-off-steps-on-prem)). `--what-if` prints the settings and runs nothing.

The controller reaches the runner over ssh with a forced command and `--from-ssh`; that key may set only the variables the controller's own steps take (`SSH_VARS` in `onprem-step.sh`). Run as root on the host, any step input is allowed. The script's header lists the other targets: `handoff`, `publish`, `scans` and `archive`.

On a `"server": "azure"` environment the same steps start with `boinc/scripts/start-boinc-bridge.sh` ([the Azure server](azure.md#starting-a-step)).

### Register the apps

An app added to `boinc/project/project.xml` needs registering on the server. Provisioning doesn't deploy the on-prem server, so run the step once the new image is in place:

```sh
usnm-boinc-step --env prod --target bridge --step register-apps --wait-mins 15
```

It copies the deployed image's `project.xml` into the project and runs `bin/xadd`, so it is safe to repeat. At the pinned BOINC commit (22bbebe), `tools/xadd` looks up each `<platform>` and `<app>` by name, inserts the missing ones, updates a changed `user_friendly_name` or `min_version`, prints "already in DB" for the rest, and never deletes or deprecates anything. `xadd` gives a new app weight 1, so the step's `register-apps.php` then sets the `<weight>` from `project.xml` on each app with no app version yet, reads it back and fails if it differs. It leaves the weights of apps with versions alone and reports any that differ.

Adding an active app changes the feeder's startup slot allocation, which neither `xadd` nor `reread_db` rebuilds: coordinate a [feeder-only refresh](feeder-refresh.md) and check [feeder visibility](../apps/app-release.md#feeder-visibility). A repeat that changes no active app needs no restart. The step needs a bridge image that has it (`boinc/scripts/build-boinc-images.sh`, then `SERVER_IMAGE` in `server.env` and a restart of `usnm-boinc-server.service`).
