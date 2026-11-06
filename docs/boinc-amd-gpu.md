# AMD native Qwen Vulkan onboarding

`usnm_qwen` plan class `amd_vulkan_8b` supports automatic eligible AMD volunteer
scheduling, including initial use of a possible Radeon RX 9070 XT (16 GB).
One RX 9070 XT on Windows has run it (below). Other AMD cards and Linux drivers
have not, and correctness, memory use and performance remain unmeasured in
[#885](https://github.com/tgoodyear/usnewsmap.com/issues/885).

This is a native application, not Docker/Podman, ROCm or a CUDA fallback. Neither
the NVIDIA CUDA Qwen containers nor the Surya CUDA images support AMD through this
change. The existing Intel/NVIDIA measurements are not AMD acceptance evidence.

Signed Linux **1.9 / 109** (app version 61) and Windows **1.10 / 110**
(app version 62) were published on 26 October 2026 with the later bounded failure
diagnostics and Windows embedded-lock fix. The earlier 1.7/1.8 versions remain
intact; admission and validation policy are unchanged. See the
[release receipt](notes/2026-10-26-boinc-gpu-release.md).
This is not a successful RX 9070 XT execution or a resolved startup-failure claim.

## Admission status and qualification

AMD admission is **general** since 1 November 2026, with the maintainer's approval
and after a dry run: `amd_vulkan_8b` has no workunit bounds, and compatible hosts
receive eligible accelerator-only work through normal BOINC scheduling without
per-volunteer cohorts or manual host approval. Promotion removed only AMD workunit
bounds, not GPU, memory or validation checks. It came after the pause in
[home-infra#81](https://github.com/tgoodyear/home-infra/issues/81), which followed
signed Windows 1.10/110 failing before inference on an actual RX 9070 XT.

The acceptance evidence is one ordinary canary. On workunit 23296 (30 October),
signed Windows 1.14 on the RX 9070 XT read 12 of 12 crops, an Apple host's reading
agreed under the strict validator, and the AMD result became canonical. Workunit
21128 (1.11, 27 October) stays inconclusive; see
[the validation note](notes/2026-11-01-qwen-cross-vendor-validation.md). That is
one validated unit on one card, not an equivalence or accuracy claim.

The published AMD versions read request version 1 only. Request versions 2 to 4 need
a newer signed AMD release; until one is published, `qwen-auto-cohort.py` closes its
window rather than admit jobs an open AMD class cannot read.

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
to verify PCI identity. On Windows drivers without `VK_EXT_pci_bus_info`, the
baseline runtime is **not sufficient**: use the identity-only patched b8799
Vulkan DLL described below, together with the corrected signed helper. Windows requires
`ggml-vulkan.dll`, the common llama/ggml/mtmd DLLs,
`ggml-cpu-x64.dll`, `libomp140.x86_64.dll`, and the Microsoft CRT enforced by
`scripts/boinc/install.py`. Keep the complete pinned Linux library set as well.
Verify that the bundled server launches and lists the real AMD Vulkan device
before admission. Retain checksums and the signed publication/readback receipt.

### Windows drivers without Vulkan PCI bus information

The actual signed Windows 1.10/110 RX 9070 XT task failed before inference with
OpenCL `????:03:00.0` and Vulkan `[]`. In baseline b8799, ggml exports no
`device_id` when `VK_EXT_pci_bus_info` is absent, even when the adapter supports
Vulkan inference. Re-signing the helper alone cannot fix that runtime limitation.
Neither a successful build nor a device listing is hardware acceptance.

The pinned source patch exports `luid:` followed by the **same physical Vulkan
device's eight LUID bytes** only when PCI information is unavailable on Windows.
The helper opens that exact LUID with `D3DKMTOpenAdapterFromLuid`, queries
`KMTQAITYPE_ADAPTERADDRESS`, closes the adapter, and compares its PCI
bus/device/function with the BOINC-assigned OpenCL topology. No enumeration
ordinal or adapter name is used as an identity. D3DKMT does not supply a PCI
domain: the helper refuses a domain-known OpenCL assignment rather than invent
domain zero. Duplicate matches, invalid/zero LUIDs, unavailable adapter APIs or
failed address queries remain refusals. Linux and extension-provided PCI
identities retain their existing path.

Build on Windows with MSVC, CMake, Python, and the upstream b8799 Vulkan SDK
version **1.4.313.2** installed (`VULKAN_SDK` set). Use an operator build
directory, not an installed BOINC slot. From this reviewed repository checkout:

```powershell
git clone --config core.autocrlf=false --no-checkout https://github.com/ggml-org/llama.cpp.git amd-llama-source
git -C amd-llama-source checkout e1a9a6dcbefccb4b864d9385ce8494f2a7f2ffcd
.\scripts\boinc\build_amd_vulkan_runtime.ps1 `
  -Source "$PWD/amd-llama-source" -Build "$PWD/amd-llama-build" `
  -Output "$PWD/amd-llama-identity"
```

The builder rejects a dirty/wrong source checkout; the patcher also verifies the
original Vulkan source SHA-256. It builds only the `ggml-vulkan` target with the
upstream release's dynamic-backend mode and verifies the DLL's
`ggml_backend_init` export before emitting
`ggml-vulkan.dll` plus `AMD_RUNTIME_BUILD.json` (source commit, patch digest,
output digest and local SDK provenance). These are **unsigned build artifacts**.
The exact patched function is tested against synthetic Vulkan responses on both
platform branches in CI; Windows CI also compiles and self-tests the helper.
This is not a full patched-runtime build or a real-card test.

For the next reviewed Windows AMD app version:

1. Verify and unpack the pinned `win-vulkan-x64` baseline archive; replace only
   `ggml-vulkan.dll` with the builder output. Keep its complete original runtime
   and model set. Retain the receipt; do not claim the resulting bundle equals
   the unmodified archive hash.
2. Use the same reviewed native CI commit's app, corrected helper, app-local CRT
   and unchanged locks. Verify every artifact hash, and test the original
   `llama-server.exe` with the patched DLL before signing.
3. Use `scripts/boinc/boinc-app-release.sh`'s existing native
   `--plan-class amd_vulkan_8b --platform windows_x86_64` on-prem release path,
   signing **all** replaced and existing extra files. Include
   `--extra-file AMD_RUNTIME_BUILD.json=PATH`. Never overlay an unsigned DLL
   into a client slot or expose signing keys. Record the new app/version IDs,
   commit and public signed-download/readback hashes.
4. Coordinate with the current `#529` runtime owner **before** publication,
   host validation or any admission change. Keep general AMD admission stopped.
   Use the bounded exact-one-workunit acceptance procedure below only with
   separately coordinated permission; require the corrected signed app download,
   exact device mapping, real offload and valid inference return. Then require
   ordinary independent-peer validation/collection before general reopening.
   A pre-inference failure or missing LUID leaves admission closed.

Publish through the normal [reviewed app release path](../contrib-compute/docs/app-release.md)
with `--app usnm_qwen --kind native --plan-class amd_vulkan_8b` and the matching
platform. Stage the unchanged Qwen3-VL-8B Q4_K_M model and Q8_0 projector from
`weights.lock` as digest-qualified sticky inputs, using `usnm_qwen_8b_in`,
native-8B batch 8, 6 GiB host-memory/18 GiB disk bounds, quorum 2 and target
replicas 2. Do not edit scientific rows or relax independent/distinct-host validation.

Only after the required acceptance above has passed and the reopening is
coordinated on #529, preview and explicitly promote **only AMD**:

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

### Required bounded acceptance: stage, audit, admit, then resume the same workunit

This procedure, or the [ordinary one-workunit canary](#ordinary-one-workunit-canary)
below, is **required** before reopening the corrected Windows release
(and remains available as a diagnostic afterwards). It deliberately
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
production promotion above is separate. For this bounded acceptance, preview and
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

AMD's bounded pilot has an **independent exact-one-workunit** window;
`--workunit-end` cannot widen it. It is deliberately not a member of the existing `shared_8b`
Intel/NVIDIA pilot. Opening/stopping AMD preserves all old windows; legacy/shared
commands (including an unqualified `--stop`) preserve AMD. Stop AMD explicitly.
All modes share the policy lock and durable journal. Image policy refresh
introduces AMD closed on upgrade and preserves explicit general, bounded or
stopped AMD state on restart.

### Ordinary one-workunit canary

The staged harness above needs a dedicated assignment-only account and a peer
host eligible for the **same exact release** (e.g. a second Windows AMD client).
When no such peer exists, an explicit ordinary canary admits AMD to one
ordinary scientific workunit instead, the same way the NVIDIA/Intel bounded
windows do:

```sh
php scripts/boinc/native-qwen-cohort.php --project /project/usnmocr \
  --plan-class amd_vulkan_8b --ordinary-canary --workunit ID
php scripts/boinc/native-qwen-cohort.php --project /project/usnmocr \
  --plan-class amd_vulkan_8b --ordinary-canary --workunit ID --apply
```

The difference is where the peer comes from. The staged harness assigns the
client and a reviewed same-release peer to a new targeted workunit. The ordinary
canary assigns nothing: the workunit's second **normal replica** goes to any
host eligible for native 8B (for example Apple or NVIDIA), with the same pinned
model, batch, crops, scientific identity and quorum, and the normal validator
compares them. It is a bounded ordinary rollout of one workunit, not a way
around the staged fixture's isolation checks.

The flag is accepted only with `amd_vulkan_8b` and one `--workunit`; it cannot be
combined with `--workunit-end`, `--stop` or `--general`. The workunit must be an
ordinary native-8B workunit: batch 8, 18 GiB disk bound, quorum 2, target 2, not
yet canonical, no error, `need_validate=0`, `transitioner_flags=0`, not bound to an
app version (`app_version_num=0`), no `assignment` rows, and exactly two unsent
results. Choose one that no other host has been sent yet. Without the flag, the
default AMD window still requires the staged no-results workunit. The plan XML
change (`min_wu_id=max_wu_id=ID`), shared lock and durable journal are the same
as the default window; the proof records `admission_mode: ordinary_canary`. It
changes no host trust, quorum, account, scientific row or other class.

Acceptance still requires all of the following before any general reopening:

- the AMD host's result ran the intended published app version (check
  `result.app_version_id`);
- the device mapping succeeded and the GPU actually offloaded;
- the result is valid; and
- an independent peer result agrees and becomes canonical.

If an older app version was chosen, or any evidence is bad, close AMD with
`--plan-class amd_vulkan_8b --stop --apply` and leave general admission closed.

#### AMD-first ordinary tail reservation

Opening AMD and NVIDIA/Intel on the same pair does **not** reserve an AMD
replica: two faster requesters can consume both before the AMD host polls.
`scripts/boinc/qwen-amd-reservation.py` provides a separate, operator-reviewed
lease for **one newest, untouched ordinary 8B pair**, currently the signed
Windows 1.14 release. It never writes BOINC science, result or assignment rows.
The existing two-unsent/quorum-2 gates remain mandatory, including zero host,
sent time, outcome, validation and app-version IDs, no assignment rows, and
`one_result_per_host_per_wu`. Unknown active Qwen plan classes fail closed.

The lease requires AMD initially closed and all NVIDIA/Intel windows closed or
bounded strictly **before** the reserved tail. It opens only that AMD workunit
and temporarily adds `max_wu_id=ID-1` to the otherwise unbounded Apple class.
All earlier eligible Apple work remains eligible; already assigned work is
untouched. This also temporarily excludes **later newly produced work from
Apple**, not just the reserved tail: it is not an arbitrary single-ID exclusion.
Do not use it as an unattended permanent cap or while earlier Apple work is
unavailable. The producer and other vendors are not paused.

Before applying, record the exact workunit, policy SHA, independent input/model/
source-quality/archive-provenance receipt, limits and rollback on #529. Install
the reviewed refresh/helper versions in both the current server and persistent
server image first. Prearm a **separate, durable host rollback unit** for this
lease, scheduled within 60–7200 seconds, and independently read back its active
state/deadline. Save that readback as JSON with `lease`, `active: true`,
`deadline` (Unix seconds), and the actual unit/evidence details. The unit must
call this helper's `release --apply --lease SAME_ID` at that deadline, retrying
short normal project-lock admission; it must not depend on the observer, DB or
collector. Do not pause, override or reuse another owner's timer.

Run each operation under the normal **short-lived project mutex**; the helper
also takes `.native-qwen-cohort.lock`. For example, inside the server container:

```sh
python3 scripts/boinc/qwen-amd-reservation.py reserve \
  --project /project/usnmocr --lease LEASE_32_HEX --workunit ID \
  --host 5 --app-version 68 --lease-seconds 5400 \
  --expected-policy-sha256 REVIEWED_POLICY_SHA \
  --cohort-helper /path/to/reviewed/native-qwen-cohort.php \
  --cohort-helper-sha256 REVIEWED_HELPER_SHA \
  --evidence /path/to/independent-per-workunit-receipt.json \
  --rollback-receipt /path/to/active-rollback-readback.json
# Add --apply only after the preview, review and live guards pass.
python3 scripts/boinc/qwen-amd-reservation.py tick \
  --project /project/usnmocr --lease LEASE_32_HEX --apply
python3 scripts/boinc/qwen-amd-reservation.py release \
  --project /project/usnmocr --lease LEASE_32_HEX --apply
```

The independent receipt uses the native feed handoff's `workunit_id`, `doc_id`,
`job_sha256`, `page_sha256`, `quality_sha256`, `provenance_sha256` and
`provenance.doc_id`. The helper rehashes the actual job/page and enforces the
existing engine, model, prompt, token budget, 1–12 crops and 8192 UTF-8-byte
contract. It retains the full receipt hash; this is **not** a fresh archive/GGUF
byte verification or a scientific accuracy claim.

The default 90-minute lease covers the observed AMD client's 3603-second normal
request interval without forcing a client RPC. The hard maximum is two hours;
the actual deadline and continuing Apple backlog must be justified on #529.
New native production is allowed during the lease. Subsequent finite NVIDIA/
Intel windows may be entirely before **or after**, but never cross, the reserved
ID; AMD polling must not block their automatic refill.

The initial policy publication is not itself an accepted reservation. In the
single-project Linux CGI container, the helper records scheduler PID/start-time
identities both before and after publication. Poll `tick` under separate short
mutex operations. Only after those old CGI processes naturally exit and a
**subsequent** DB read still finds the untouched pair can its receipt say
`old_schedulers_drained_and_pair_untouched: true`. No CGI is killed or paused.
If an old request wins instead, `tick` removes only its owned Apple ceiling and
closes its AMD window; it never takes the assigned result back. A PID reuse is
not mistaken for an old scheduler. FastCGI/persistent schedulers require a
different reviewed drain mechanism; this procedure does not qualify them.

On actual host/app assignment, collision, lease expiry or explicit release, the
same helper restores Apple and closes its AMD window without reverting other
vendors' newer finite windows. The independent deadline unit calls `release`,
which does **not** require a working DB. Journals and staged policy writes are
fsynced before atomic replacement. A startup refresh preserves an unexpired
owned ceiling, or removes it and closes that AMD window when expired. Other
cohort operations refuse to open competing windows while the lease is present.
The normal peer is then free to run; all candidate capture, generation metrics,
independent validation and image-grounded quality acceptance remain required.
Assignment alone does not resolve home-infra#81 or authorize general admission.

#### Inconclusive canary: one-replica tiebreaker

If the canary and its peer are both successful but **inconclusive**, the
transitioner raises `target_nresults` and creates one more unsent replica. When
every other native class is closed, no host may be eligible for it. An explicit
tiebreaker admits Intel or NVIDIA to that one workunit only:

```sh
php scripts/boinc/native-qwen-cohort.php --project /project/usnmocr \
  --plan-class intel_vulkan_8b --tiebreaker --workunit ID
php scripts/boinc/native-qwen-cohort.php --project /project/usnmocr \
  --plan-class intel_vulkan_8b --tiebreaker --workunit ID --apply
```

The flag is accepted only with `intel_vulkan_8b`, `nvidia_vulkan_8b` or
`shared_8b` and one `--workunit`. The workunit must be ordinary native 8B (batch
8, 18 GiB disk bound, quorum 2, not canonical, no error, `need_validate=0`,
`transitioner_flags=0`, `app_version_num=0`, no `assignment` rows), with
`target_nresults` equal to its result count, exactly one unsent replica, and every
other result successful and inconclusive (`validate_state=4`). The live
project `config.xml` (and any private `--db-project` copy) must enable
`<one_result_per_host_per_wu/>` (or `1`) exactly once in its single `<config>`
block, so the scheduler sends the replica to a host other than the inconclusive
ones. The mode edits no result,
quorum or scientific row: the normal validator compares the new result with the
existing ones. The plan XML change, lock and journal match the bounded window;
the proof records `admission_mode: tiebreaker`. Close the class with `--stop` once
the replica is sent. The default and ordinary-canary modes still refuse
workunits that already have results.

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
backend's PCI identities (or the verified Windows LUID-to-PCI bridge above),
returning exactly one `VulkanN`, regardless of order.
AMD topology lacks a domain, so matches across multiple domains are ambiguous
and refused. Conflicting AMD platform mappings, missing PCI support, duplicate
identities, or an invisible assigned adapter are also refused, even if another
AMD card is visible. There is no name-only or ordinal fallback. The Rust app
also requires that exact device in the server listing with a real AMD/ATI/Radeon
or Advanced Micro Devices name, skipping software and other vendors.

The signed helper runs with the same bounded 300-second GPU-enumeration deadline
as `llama-server --list-devices` and the same bundled backend
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
