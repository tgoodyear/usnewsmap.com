# AMD native Qwen Vulkan onboarding

`usnm_qwen` plan class `amd_vulkan_8b` supports automatic eligible AMD volunteer
scheduling, including initial use of a possible Radeon RX 9070 XT (16 GB).
**Initial volunteer use is hardware-unverified:** no RX 9070 XT was available
for this change. Driver compatibility, correctness,
memory use and performance on that card remain unverified in
[#885](https://github.com/tgoodyear/usnewsmap.com/issues/885).

This is a native application, not Docker/Podman, ROCm or a CUDA fallback. Neither
the NVIDIA CUDA Qwen containers nor the Surya CUDA images support AMD through this
change. The existing Intel/NVIDIA measurements are not AMD acceptance evidence.

## Automatic scheduling and optional qualification

The production admission mode is automatic: an attached host with a
compatible accelerator should receive eligible accelerator-only work through
normal BOINC scheduling, without a manual admission step for each volunteer.
The maintainer authorized initial AMD admission before hardware qualification;
signed publication plus one operator **platform-wide promotion** enables it.
That promotion removes only AMD workunit bounds, not GPU, memory or validation
checks. Volunteers do not need an individual cohort, audited workunit or manual
host approval. The optional bounded procedure below remains a diagnostic tool,
not a prerequisite for normal volunteers.

GPU-only Qwen routes use separate resource classes: AMD/ATI and Intel are
detected through OpenCL for Vulkan execution; NVIDIA uses CUDA detection for
its CUDA/Vulkan routes, and Apple uses its native GPU route. A host still needs
a published app version for its OS, the correct model/batch, supported BOINC
and driver capabilities, sufficient memory, and the actual assigned device.
GPU classes never silently fall back to CPU. Existing Intel/NVIDIA native
cohort windows are also bounded pilots, not general automatic queue admission.

Automatic admission does not manufacture work. Matching native Qwen 8B batch-8
work with ordinary replicas must be available in the accelerator-only queue;
CPU batch-12 and NVIDIA-container batch-18 work are not eligible for AMD.
Correctness, performance and actual driver compatibility remain to be measured,
and failed GPU probes remain real failures rather than CPU fallback or accepted
readings.

## Volunteer prerequisites

- **OS/artifact:** x86-64 Linux (`x86_64-pc-linux-gnu`) or x64 Windows
  (`windows_x86_64`) only. ARM Linux and both macOS platforms are excluded.
  The native driver comes from `boinc-quality-native.yml`; its existence alone
  is not a published AMD app version. The operator must assemble and sign the
  matching Vulkan bundle before general admission.
- **Linux driver:** a kernel/firmware stack and real AMD Vulkan ICD supporting
  the card, plus a compatible AMD OpenCL runtime so BOINC detects the AMD/ATI
  resource. Vulkan availability alone does not make a host scheduler-eligible.
  Confirm `vulkaninfo --summary`, `clinfo`, and the BOINC event log all identify
  the real card, including under the BOINC service account. Mesa RADV may provide
  Vulkan, but does not by itself establish working OpenCL detection. No specific
  RX 9070 XT Linux driver/runtime combination has been accepted yet. The host must
  also satisfy the CI-built helper's glibc/libstdc++ runtime requirements; verify
  both native executables launch before admission.
- **Windows driver:** a current AMD graphics driver supporting the card, with
  Vulkan and OpenCL available to BOINC. The driver supplies `vulkan-1.dll`; do not
  copy an arbitrary loader into the application bundle. The signed native bundle
  must include its app-local Microsoft CRT and pinned llama runtime DLLs.
- **BOINC:** stable 8.2.11 or newer, reporting an AMD/ATI GPU and at least
  7,000 MB device RAM. Keep ordinary GPU-use preferences under the volunteer's
  control; no account flag, attachment change or preference edit enables this class.
- **Memory/disk:** the 16 GB device capacity is not a guarantee of free memory.
  The app refuses below 7,000 MiB free Vulkan memory. Leave room for the desktop
  and other workloads, run one Qwen task at a time for acceptance, and leave
  adequate host RAM headroom (16 GiB host RAM is a conservative starting point,
  not an AMD measurement). The native 8B cold disk envelope is 18 GiB;
  normal BOINC free-space reserves still apply.

## Operator publication and general admission

Use the unchanged `ocr/boinc/qwen/llama.lock` b8799
`ubuntu-vulkan-x64` or `win-vulkan-x64` asset, verified by its pinned SHA-256.
Do not substitute the CPU or CUDA archive. Bundle `llama-server[.exe]`, all its
runtime shared libraries, `llama.lock`, `weights.lock`, and the matching native
CI artifact's `usnm-amd-device-map[.exe]` as signed native extra files. The helper
is required for AMD only and uses the unchanged b8799 ggml device-properties ABI
to verify PCI identity; it does not rebuild or replace llama.cpp. Windows requires
`ggml-vulkan.dll`, the common llama/ggml/mtmd DLLs,
`ggml-cpu-x64.dll`, `libomp140.x86_64.dll`, and the Microsoft CRT enforced by
`scripts/boinc/install.py`. Keep the complete pinned Linux library set as well.
Verify that the bundled server launches and lists the real AMD Vulkan device
before admission. Retain checksums and the signed publication/readback receipt.

Publish through the normal [reviewed app release path](../contrib-compute/docs/app-release.md)
with `--app usnm_qwen --kind native --plan-class amd_vulkan_8b` and the matching
platform. Stage the unchanged Qwen3-VL-8B Q4_K_M model and Q8_0 projector from
`weights.lock` as digest-qualified sticky inputs, using `usnm_qwen_8b_in`,
native-8B batch 8, 6 GiB host-memory/18 GiB disk bounds, quorum 2 and target
replicas 2. Do not edit scientific rows or relax independent/distinct-host validation.

After publication, under the normal #529 controller guard, preview and explicitly
promote **only AMD**:

```sh
php scripts/boinc/native-qwen-cohort.php --project /project/usnmocr \
  --plan-class amd_vulkan_8b --general
php scripts/boinc/native-qwen-cohort.php --project /project/usnmocr \
  --plan-class amd_vulkan_8b --general --apply
```

The existing durable journal and shared policy lock cover this change. The
promotion atomically removes only AMD `min_wu_id`/`max_wu_id` and records the XML
comment `usnm-amd-general-admission-v1`. The marker is operator state, not a
BOINC preference or host trust override. Startup refresh preserves this explicit
mode while restoring reviewed resource gates. Removing bounds without that marker
does **not** opt in: refresh restores closed bounds. An explicit AMD `--stop`
removes the marker and restores `2/1`, which remains stopped across restarts.
Neither general promotion nor stop changes any Intel/NVIDIA window. The deployed
helper is also available at `/usr/local/lib/usnm-boinc/amd-885/native-qwen-cohort.php`
(the `amd-885` and `live-overlay` server images under `contrib-compute/images/server/`
install it).

### Optional bounded diagnostic: stage, audit, admit, then resume the same workunit

This procedure is not required for automatic scheduling. It deliberately
replaces general AMD admission with an exact-one-workunit pilot; coordinate any
such platform-wide diagnostic with other AMD users and close AMD first.

AMD acceptance does **not** use the runner's legacy create-on-dispatch path.
Prepare a reviewed server fixture with the exact release/platform/plan, a
dedicated assignment-only account, one distinct existing peer host, all four
verified native-8B physical input names/hashes, native templates and output
contracts. Set `batch: 8`, `min_quorum: 2`, and profile fields `gpu: "amd"`,
`memory_bytes: 6442450944`, `disk_bytes: 19327352832`. Initially omit
`audited_workunit_id`. Use a distinct numeric app version for this platform/plan;
staging/reservation reject ambiguous app/version routing.
The peer must be eligible for the same exact registered release; this path does
not relax the existing app-version or distinct-host validation gates.

While AMD is still closed, explicitly stage that fixture through the server adapter:

```sh
python3 scripts/boinc/acceptance_server.py --project /project/usnmocr \
  --fixtures /project/acceptance --stage-fixture REVIEWED_FIXTURE
```

This hashes the existing inputs, verifies the native template bounds and release,
and uses `create_work --no_new_results` with the exact numeric version and no
host assignment. This requires the reviewed server base containing
`patch-server-stage-work.py`; an older tool rejects the flag, leaving a durable
uncertain-stage journal for reconciliation, not a retry.
It journals intent before creation, never opens admission, and returns
`audited_workunit_id`. Do not repeat after an uncertain outcome: reconcile the
private `.stage.json` journal and database identity first. Audit **zero results**,
`transitioner_flags=2` (`TRANSITION_NO_NEW_RESULTS`), a future `transition_time`,
no pending validation, and the workunit's ID/name,
payload, template, batch, bounds and version, then add that ID to the same fixture
as `audited_workunit_id` and use it as `ID` below.

The installation default is **closed** (`min_wu_id=2`, `max_wu_id=1`); normal
production promotion above is separate. For this optional diagnostic, preview and
apply one newly staged, audited, unaccepted workunit without generic replicas:

```sh
php scripts/boinc/native-qwen-cohort.php --project /project/usnmocr \
  --plan-class amd_vulkan_8b --workunit ID
php scripts/boinc/native-qwen-cohort.php --project /project/usnmocr \
  --plan-class amd_vulkan_8b --workunit ID --apply
```

Now run the existing-client acceptance runner with that fixture/config and exact
release (see [release acceptance](../contrib-compute/docs/app-release.md)).
Reservation verifies the audited ID/name, input references, native bounds,
unaccepted scientific state and targeted staging without results against the
**exact** operator window. Dispatch rechecks those gates under database locks,
adds only the client and reviewed peer host assignments to that same workunit,
and journals intent; it never creates a replacement workunit. Closing the window
after reservation prevents dispatch. Dispatch holds the shared cohort-policy
lock from the admission recheck through assignment commit: a concurrent operator
stop or policy refresh cannot complete in the middle of that interval. If stop
wins the lock first, dispatch observes the closed policy and refuses.
Status, independent validation and canonical
collection continue to track the reserved workunit identity. Finally:

```sh
# Close only AMD; allow in-flight results to report.
php scripts/boinc/native-qwen-cohort.php --project /project/usnmocr \
  --plan-class amd_vulkan_8b --stop --apply
```

AMD's optional pilot has an **independent exact-one-workunit** window;
`--workunit-end` cannot widen it. It is deliberately not a member of the existing `shared_8b`
Intel/NVIDIA pilot. Opening/stopping AMD preserves all old windows; legacy/shared
commands (including an unqualified `--stop`) preserve AMD. Stop AMD explicitly.
All modes share the policy lock and durable journal. Image policy refresh
introduces AMD closed on upgrade and preserves explicit general, bounded or
stopped AMD state on restart.

The scheduler uses `<gpu_type>ati</gpu_type><opencl/>` and reserves one AMD GPU
plus two host CPUs. The pinned BOINC source accepts `ati`/`amd` in
[`plan_class_spec.cpp`](https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/plan_class_spec.cpp#L690-L750)
and maps `amd` plan names to `PROC_TYPE_AMD_GPU` in
[`sched_util_basic.cpp`](https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/sched_util_basic.cpp#L313-L332).
Inference uses Vulkan, not OpenCL. The staging CLI patch exposes BOINC's existing
targeted-work flag without creating an assignment before audit/admission.
The ordinary transitioner must not create generic replicas: the pinned
`sched_assign.cpp::need_targeted_instance()` subtracts unsent replicas from the
target of two. With zero replicas initially, targeted scheduling creates one
result for the client and one for the reviewed peer, preserving quorum/target
2 and all native bounds. Do not delete, cancel, relabel or replay existing
scientific rows to convert an old generic fixture; stage a new reviewed identity
while AMD is closed instead.

The app requires an explicit ATI/AMD assignment, nonnegative `gpu_device_num`,
and `gpu_opencl_dev_index`. The physical BOINC device number is **not** assumed
to equal the index among visible Vulkan devices. Following the pinned
`boinc_get_opencl_ids_aux` convention, the helper resolves the assigned OpenCL
GPU index on AMD platforms and obtains its PCI identity (`cl_khr_pci_bus_info`
or AMD PCI topology). It matches that identity against the bundled ggml Vulkan
backend's PCI identities, returning exactly one `VulkanN`, regardless of order.
AMD topology lacks a domain, so matches across multiple domains are ambiguous
and refused. Conflicting AMD platform mappings, missing PCI support, duplicate
identities, or an invisible assigned adapter are also refused, even if another
AMD card is visible. There is no name-only or ordinal fallback. The Rust app
also requires that exact device in the server listing with a real AMD/ATI/Radeon
or Advanced Micro Devices name, skipping software and other vendors.

The signed helper runs with a 30-second deadline and the same bundled backend
and loader environment as the server. Its registry loader comes from
`libggml.so` / `ggml.dll`, while device APIs come from
`libggml-base.so` / `ggml-base.dll`. Both libraries and the Vulkan backend must
be included and signed in the native bundle on Linux and Windows.
A missing helper/assignment, unverifiable
identity, failed probe or insufficient free memory ends `no_gpu`; it never
selects another adapter or falls back to CPU. Driver support for the required
PCI properties remains a real per-task requirement, not an assumption or a
waived check for initial volunteer use.

## Pending actual-device acceptance

On a dedicated, explicitly authorized test client, retain evidence of:

1. Signed AMD app download, exact driver/OS/BOINC versions, archive/model hashes,
   and BOINC's AMD resource assignment.
2. The bundled server's hardware listing and actual `--device VulkanN`
   selection, layer offload/GPU activity, free/peak VRAM and host RAM.
3. Successful native inference, checkpoint/return and output collection; not
   merely device enumeration or app-version registration.
4. Independent validation against another host and canonical collection under
   the existing quorum/distinct-host rules, without accepting failures as reads.
5. Correctness on retained benchmark inputs/readings, elapsed/load/inference
   times, memory evidence, and any mismatch or driver failure.

The acceptance runner uses `gpu: "amd"` in a reviewed fixture and maps BOINC's
`coproc_ati` resource accordingly. Use existing-client mode; Azure ephemeral
acceptance supports CPU/NVIDIA only and must not be repurposed for AMD.
Its scheduler preflight refuses the closed class and never opens cohorts.
Close the AMD window after the canary. Do not claim speed, equivalence or general
RX 9070 XT support until the real-card evidence is reviewed in #885.
