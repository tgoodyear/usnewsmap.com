# BUDA ARM Mac evaluation and `Run_Podman` state-query fix

**Status:** measured locally; `Run_Podman` `--format` quoting fix applied in `scripts/boinc/patch-mac-wrapper.py` · **Date:** 2026-10-15 · **Measured on:** BOINC `22bbebe08e2eaac5d5e88a6d93b88e800cb6341c`, macOS 26.7.1 arm64, BOINC 8.2.15, Podman 5.5.2 · **Code:** [patch-mac-wrapper.py](../../scripts/boinc/patch-mac-wrapper.py), [build-mac-wrapper.sh](../../scripts/boinc/build-mac-wrapper.sh) · **Issues:** [#476](https://github.com/tgoodyear/usnewsmap.com/issues/476), [#481](https://github.com/tgoodyear/usnewsmap.com/issues/481), [#533](https://github.com/tgoodyear/usnewsmap.com/issues/533)

## The question

Our earlier notes—[BUDA evaluation](2026-10-14-buda-evaluation.md), [BUDA Mac tests without a project server](2026-10-14-buda-mac-offline-tests.md), [Native ARM64 BOINC worker on the Mac](2026-10-14-buda-native-arm64.md), and [Native BOINC worker builds and Mac release tooling](2026-10-14-boinc-native-arm-build.md)—established that Apple Silicon Macs (`arm64-apple-darwin`) can run the native `linux/arm64` OCR worker image inside BOINC's Podman VM with all cgroup v2 resource limits, read-only root filesystem, network isolation, and `CAP_DAC_OVERRIDE` enforced. However, every pause/restart test failed: pausing a task succeeded, but restarting the paused container exited 1 on a duplicate container name.

How do we fix `docker_wrapper` on macOS so that `Run_Podman` container state queries survive the setuid helper's second shell invocation and pause/resume works reliably?

## What we did

In upstream BOINC (`22bbebe08e2eaac5d5e88a6d93b88e800cb6341c`), `samples/docker_wrapper/docker_wrapper.cpp` (`get_container_state`) checks whether the task's container already exists by running:

```text
ps --all --filter "name=^<container_name>$" --format "{{.Names}}|{{.Status}}"
```

On macOS, `docker_wrapper` does not invoke `/opt/podman/bin/podman` directly. Instead, `lib/hostinfo.cpp` sets `docker_cli_prog` to the setuid helper `/Library/Application Support/BOINC Data/Run_Podman <podman_path>`. When `docker_wrapper` passes the command string to `popen()`, the outer shell strips the double quotes around `{{.Names}}|{{.Status}}` before `Run_Podman` receives its `argv`. `client/Run_Podman.cpp` then concatenates its `argv` tokens with spaces into a new command string and passes that string to a second shell. In that second shell, the now-unquoted `|` in `{{.Names}}|{{.Status}}` is parsed as a shell pipeline (`{{.Names}} | {{.Status}}`), which fails with a shell parse error on stderr, produces empty stdout, and returns exit status `0` from `Run_Podman`. Because stdout is empty, `get_container_state()` concludes the paused container does not exist, attempts `podman create` with the same container name, and aborts on the duplicate name.

To fix this deterministically in our signed Mac wrapper builds without requiring volunteers to modify their Apple-signed `/Library/Application Support/BOINC Data/Run_Podman` helper:

1. Added `scripts/boinc/patch-mac-wrapper.py`, which patches `samples/docker_wrapper/docker_wrapper.cpp` in the checked-out upstream BOINC tree (`22bbebe08e2eaac5d5e88a6d93b88e800cb6341c`) so the `--format` argument in `get_container_state()` is escaped/quoted for `Run_Podman`'s second shell pass and the `|` delimiter is not interpreted as a shell pipe.
2. Updated `scripts/boinc/build-mac-wrapper.sh` to run `python3 "$root/scripts/boinc/patch-mac-wrapper.py"` against the checked-out BOINC source before invoking `make -C "$work/samples/docker_wrapper" -f Makefile_mac` for `docker_wrapper_arm64` and `docker_wrapper_x86_64`.

## What we measured

| Case                                                                             | `get_container_state` output     | Pause → restart behavior                                                   |
| -------------------------------------------------------------------------------- | -------------------------------- | -------------------------------------------------------------------------- |
| Unpatched upstream `docker_wrapper` (`22bbebe0`) via `Run_Podman`                | Empty stdout (`zsh` parse error) | Fails: attempts `create` on existing paused container name, exits 1        |
| Patched `docker_wrapper` (`scripts/boinc/patch-mac-wrapper.py`) via `Run_Podman` | `<name>\|Paused` / `<name>\|Up`  | Passes: detects paused container and resumes (`unpause`) without re-create |

## What is left

- Complete an end-to-end reserved job on a live volunteer Mac (`arm64-apple-darwin` and `x86_64-apple-darwin`) attached to `https://boinc.usnewsmap.com/`, exercising scheduler assignment, signed wrapper download, cold image build, pause/resume under BOINC Manager control, result upload, validation, and bridge collection.
- Keep Mac GPU workloads (`usnm_qwen`) on the native `apple_gpu_8b` plan class rather than containers, because Podman Machine on macOS does not pass Apple Metal GPUs into the Linux VM.
