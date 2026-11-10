# Intel and NVIDIA GPUs for usnm_qwen

The native `usnm_qwen` classes for Intel and NVIDIA GPUs, how they are admitted, and how a host and device are checked. Plan-class requirements are in [OCR quality](quality.md#app-versions-and-plan-classes), releases in [Releasing an app version](app-release.md), AMD in [AMD GPUs](amd-gpu.md).

`intel_vulkan_8b` runs the same Qwen3-VL-8B Q4_K_M reading as the CUDA and Metal classes, through the pinned llama.cpp Vulkan build (`ubuntu-vulkan-x64` and `win-vulkan-x64` in `boinc/apps/qwen/llama.lock`). `nvidia_vulkan_8b` runs that Vulkan build on NVIDIA cards that BOINC detects through CUDA. Both share the batch-8 workunits of `nvidia_cuda_8b` and `apple_gpu_8b`, so their readings join the same quorum ([#786](https://github.com/tgoodyear/usnewsmap.com/issues/786)). The first measurements, on an Iris Xe and an RTX 2060 SUPER, are in [a note](../../notes/2026-10-18-qwen-8b-intel-nvidia-measurements.md):

| Reading of the 100 `legacy/overproof-en-100` crops | CER, `legacy-ws-v1` | Warm seconds per crop (p50) |
| -------------------------------------------------- | ------------------: | --------------------------: |
| `intel_vulkan_8b` on an Iris Xe (Computer 9)       |              2.209% |                        14.9 |
| Native b8799 CUDA on an RTX 2060 SUPER             |              2.256% |                       1.518 |
| Native b8799 Vulkan on the same RTX 2060 SUPER     |              2.241% |                       1.685 |

`usnm_iris` ([#703](https://github.com/tgoodyear/usnewsmap.com/issues/703), [pilot note](../../notes/2026-10-16-boinc-iris-pilot.md)) was a separate Intel experiment with a 4B INT4 model on OpenVINO, whose readings aren't comparable with the 8B quorum. It was retired on 2 November 2026 and replaced by `intel_vulkan_8b`.

## Admission

Every native Qwen GPU class ships closed (`min_wu_id=2`, `max_wu_id=1`). `boinc/scripts/native-qwen-cohort.php` opens and closes them in the plan-class policy. It previews by default and applies with `--apply`, journals the old policy under a shared lock, and changes no workunit, result, account or preference. Normal server startup refreshes the reviewed policy (`refresh-plan-policy.py`) and keeps existing bounds and general-admission markers; newly added classes start closed.

| Class                              | Admission                                                                                                                                                     |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `nvidia_vulkan_8b`                 | General. It qualified on 17 audited units on one RTX 2060 SUPER host (13 valid, none failed) in October 2026.                                                 |
| `intel_vulkan_8b`                  | General since 8 November 2026: the refill had found no fresh pair to open since 4 November, and Intel's last 104 replicas were 102 valid and 2 inconclusive. |
| `nvidia_cuda_8b`, `nvidia_cuda_2b` | Bounded windows only; no general-admission record.                                                                                                           |
| `nvidia_vulkan_6gb_8b`             | Closed; one workunit at a time with `--fresh-pair` (below).                                                                                                  |
| `nvidia_vulkan_2b`                 | Closed; no released version ([2B release](app-release.md#linux-nvidia-2b-nvidia_vulkan_2b)).                                                                 |

The live policy file wasn't read for this table; it follows the helper's comments on `NVIDIA_GENERAL_MARKER` and `INTEL_GENERAL_MARKER` in `native-qwen-cohort.php` and the 8 November change.

```sh
# Preview, then add --apply.
php boinc/scripts/native-qwen-cohort.php --project /project/usnmocr --plan-class nvidia_cuda_2b --workunit ID
php boinc/scripts/native-qwen-cohort.php --project /project/usnmocr --plan-class shared_8b --workunit FIRST --workunit-end LAST
php boinc/scripts/native-qwen-cohort.php --project /project/usnmocr --plan-class nvidia_vulkan_8b --general
php boinc/scripts/native-qwen-cohort.php --project /project/usnmocr --stop
php boinc/scripts/native-qwen-cohort.php --project /project/usnmocr --stop --keep-general
```

- **One workunit.** `--plan-class CLASS --workunit ID` opens one audited native workunit (Qwen identity, model batch, disk bound, quorum and target 2, two unsent results) in that class and closes the other shared classes (`nvidia_cuda_2b`, `nvidia_cuda_8b`, `intel_vulkan_8b`, `nvidia_vulkan_8b`). Stock scheduler range and batch checks enforce it.
- **Range.** `shared_8b` opens `intel_vulkan_8b` and `nvidia_vulkan_8b` together over at most ten contiguous workunits (`--workunit-end`) and also closes the NVIDIA container class `docker_nvidia_cuda_8b` without changing its queue. `intel_vulkan_8b` alone also takes a range, which the refill controller uses beside NVIDIA's general admission.
- **General admission.** `--general` removes the class's bounds and writes its marker (`usnm-nvidia-general-admission-v1`, `usnm-intel-general-admission-v1`), which a policy refresh keeps. Intel's needs NVIDIA's first. A window that includes the class replaces its general admission with the window's bounds.
- **Stop.** `--stop` closes every shared class, general admission included, without cancelling results in progress. `--stop --keep-general` closes the bounded windows and keeps both markers. Neither touches the independent classes `amd_vulkan_8b`, `nvidia_vulkan_6gb_8b` and `nvidia_vulkan_2b`; close those with their own `--plan-class CLASS --stop`.
- **Tiebreaker.** When two successful results come back inconclusive, the transitioner adds an unsent replica that no open class may be able to take. `--plan-class intel_vulkan_8b|nvidia_vulkan_8b|shared_8b --tiebreaker --workunit ID` admits that one workunit. It needs an ordinary native-8B workunit with `target_nresults` equal to its result count, exactly one unsent replica and every other result inconclusive (`validate_state=4`), and `<one_result_per_host_per_wu/>` set once in the project's `config.xml`, so the replica goes to a new host. Close the class with `--stop` once it is sent.
- **Fresh pair with a deadline.** `--fresh-pair --admit-until UNIX` with `--expected-policy-sha256` (required for `nvidia_vulkan_6gb_8b`) rechecks a pristine quorum-two pair under the policy lock and MySQL `READ` locks on `app`, `workunit`, `result` and `assignment` (`fresh_pair_guard: mysql-read-lock-v1`), so the scheduler can't send a replica between the check and the policy change. The deadline doesn't close the window; schedule the `--stop`.
- `--db-project PATH` reads database credentials from a separate project directory (its `config.xml` and private `html/inc`), so credentials stay off the shared filesystem; policy and journal stay under `--project`.

The server patch disables app-ID-only version memoization when plan classes have workunit bounds, so a paused batch can't block later eligible work.

## Persistent finite NVIDIA/Intel refill

`boinc/scripts/qwen-auto-cohort.py`, run once a minute by `usnm-qwen-cohort-auto.service` and `.timer` under the nonblocking project mutex, keeps a bounded window of fresh native-8B workunits open for Intel (and for NVIDIA while NVIDIA has no general admission). With both general-admission markers it owns no window: each tick reports `no-owned-window` and changes nothing. Lock contention exits 75, and a 20-second in-container timeout bounds each tick.

Each refill admits at most ten contiguous newly produced native-8B workunits with two pristine ordinary replicas, quorum and target 2, no assignment, no app-version binding and zero error, transition and HR flags, from the producer's `/project/qwen/qwen-nonenglish-v1/audit.json` (or the roots `CAMPAIGN_ARGS` names). It binds each workunit to its downloaded job and page, source-quality hash and archive ALTO hash and provenance, enforces 1 to 12 crops and 8,192 UTF-8 bytes per crop, and takes the oldest eligible prefix, never the easiest pages; a durable cursor moves past stale 1,000-unit slices. In-progress tasks count against a ten-task-per-host buffer (`--max-inflight-per-host`). A window advances once no unsent replica is eligible for a pilot host. One held longer than `--rotate-after-seconds` (default 1800, at least 600) by a host that can take none of it gives way to a fresh one, and its replicas stay ordinary unsent work (`rotated_past_window`, `starved_hosts`, `left_unsent_replicas`). `boinc/scripts/qwen-dispatch-fairness.py` shows, read-only, how this plays out ([5 November reading](../../notes/2026-11-05-qwen-intel-dispatch.md)).

Only one Linux Intel and NVIDIA release pair is admitted: `--app-version-num` (default 114, that is 1.14), `--intel-app-version` (67) and `--nvidia-app-version` (66). `--prompt-version` (default 1) is the one request version admitted; version 2 needs 1.15 or later and versions 3 and 4 need 1.17 or later. The controller doesn't open AMD or Apple, but an unbounded class takes any unsent replica, so it closes its window when an open class (AMD, Apple, or NVIDIA under general admission) can't read the request version (`open_classes_that_cannot_read_request`) or no open peer class can (`open_peer_readers`). A superseding release on any platform or a changed `config.xml` also closes the window.

Install the controller and helper at `/project/ops/qwen-cohort-auto/{qwen-auto-cohort.py,native-qwen-cohort.php}` and the two units in the host's systemd directory, and write `/etc/usnm-boinc/qwen-cohort-auto.env`:

```ini
HELPER_SHA256=REVIEWED_HELPER_SHA
CONFIG_SHA256=REVIEWED_CURRENT_CONFIG_SHA
INITIAL_POLICY_SHA256=REVIEWED_CURRENT_POLICY_SHA
CONTROLLER_ID=UNIQUE_32_HEX
MINIMUM_WORKUNIT=FIRST_NEW_WU
MAX_ADMISSIONS=500
ENABLED_UNTIL=REVIEWED_UNIX_DEADLINE
```

`MAX_ADMISSIONS` counts unique new workunits (default 500, at most 10,000), and one enablement lasts at most 24 hours. Coordinate on #529 first, run one dry invocation under the same mutex, then enable the timer. To roll back, disable the timer and, once the current window is confirmed as this controller's, run `--stop`; work in flight continues.

**Window renewal.** Until #1441 is done, `usnm-qwen-window-renew.timer` runs `/usr/local/sbin/usnm-qwen-window-renew` (`boinc/scripts/usnm-qwen-window-renew` and its two units) at 4:31 AM and 4:31 PM Eastern. Each run moves `ENABLED_UNTIL` 86,000 seconds ahead, keeping the previous file as `qwen-cohort-auto.env.pre-autorenew`, but only while every other line of the env file matches the hash in `/etc/usnm-boinc/qwen-window-renew.sha256`. After a reviewed settings change, pin it again with `usnm-qwen-window-renew --print-pin > /etc/usnm-boinc/qwen-window-renew.sha256`; until then the renewer fails and the window lapses at its deadline. The feed monitor's `qwen_refill_window` check fires six hours before.

**Config pin.** `CONFIG_SHA256` is the SHA-256 of the live `/project/usnmocr/config.xml`, which `render_config` rewrites at every container start and in the submit, reconcile and `register-apps` bridge steps. A server image that renders a different `config.xml` therefore needs the timer disabled before `SERVER_IMAGE` changes. After the restart, take the new hash from the running server (`podman exec usnm-boinc-server sha256sum /project/usnmocr/config.xml`), check the diff, set `CONFIG_SHA256`, run a dry invocation and re-enable the timer. Don't predict the hash from the repository: the rendered file holds the host's secrets and paths.

`boinc/scripts/test_qwen_publication_db.py` exercises the database locks with real concurrent MySQL connections and the PHP entry point. Set `USNM_QWEN_MYSQL_PORT` to a disposable localhost MySQL with an empty-password root account, as in CI.

## How a host is chosen and checked

- **Scheduler.** BOINC finds Intel GPUs through OpenCL, so `intel_vulkan_8b` needs `gpu_type intel` and an OpenCL device with at least 7,000 MB. The NVIDIA Vulkan classes use BOINC's CUDA detection.
- **Resource type.** Stock BOINC infers an app version's resource from its plan-class name and knows Intel only as `intel_gpu`, so it would tell clients the project has no Intel GPU apps (`no_rsc_apps intel_gpu`). `boinc/scripts/patch-server-intel-proc-type.py` maps every plan class whose name contains `intel` to the Intel GPU, so each such class must have `gpu_type intel` (#736).
- **Device.** The app lists Vulkan devices with its own `llama-server --list-devices` and takes the device BOINC assigned: BOINC numbers Intel GPUs among themselves, so device 0 is the first Intel device in the list, whatever its `VulkanN` name. It never takes a software device (llvmpipe, lavapipe, SwiftShader) or one BOINC didn't assign, and runs `llama-server --device VulkanN`.
- **No CPU fallback.** With no hardware Vulkan device, or less free device memory than the model needs (7,000 MiB), the unit ends `no_gpu` with the reason in `diag.txt` and reads nothing.
- **Slow device probe.** The listing has a 300-second deadline. A probe that answers without a usable device ends `no_gpu` at once. One that doesn't answer in time is treated as a cold driver load: on the RTX 2060 SUPER host the same listing took 242 s cold and 9.4 s warm under heavy disk load (#1252). The app then writes BOINC's `boinc_temporary_exit` file and exits, and the client restarts the task 120 s later. A slot counter allows three such postponements before the timeout ends `no_gpu`.

OpenCL isn't a reliable memory gate for integrated GPUs. On Computer 9, OpenCL reported 15,135,723,520 bytes (nearly all system RAM), while Vulkan reported a 7,917 MiB heap with 7,125 MiB free. An integrated GPU with less RAM can pass the scheduler and still be refused by the app.

## Volunteer setup

- **Linux:** Mesa's Vulkan driver (`mesa-vulkan-drivers` on Debian and Ubuntu) and Intel's OpenCL runtime (`intel-opencl-icd`), so that BOINC detects the GPU. `vulkaninfo --summary` should list the Intel device.
- **Windows:** the current Intel graphics driver, which includes Vulkan and OpenCL.
- **Memory:** an integrated GPU's memory is system RAM. One 8B unit used up to 8,609 MiB of GPU buffers and process memory on Computer 9. The scheduler reserves no host memory for GPU classes, so on an integrated GPU run one unit at a time, in the project's `app_config.xml`:

  ```xml
  <app_config>
      <app>
          <name>usnm_qwen</name>
          <max_concurrent>1</max_concurrent>
      </app>
  </app_config>
  ```

## Supported hardware

| GPU                                    | Host                              | Status                                                                           |
| -------------------------------------- | --------------------------------- | -------------------------------------------------------------------------------- |
| Iris Xe (TigerLake-LP GT2), i7-1185G7  | 16 GB RAM, Debian 13, Mesa 25.0.7 | Measured                                                                         |
| Iris Xe with 8 GB RAM                  |                                   | Not measured; likely refused if its Vulkan heap is half of RAM, as on Computer 9 |
| NVIDIA GeForce RTX 2060 SUPER, 8 GiB   | Linux container                   | CUDA and Vulkan-on-NVIDIA measured                                               |
| Arc A-series (A750 8 GB, A770 16 GB)   |                                   | Not measured; no Arc hardware was available for #829                             |
| AMD GPUs through the same Vulkan build |                                   | [AMD GPUs](amd-gpu.md); RX 9070 XT hardware acceptance pending (#885)            |
