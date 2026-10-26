# Initial Intel and AMD BOINC acceptance follow-through

On 26 October 2026 UTC, actual work followed the
[signed GPU publications](2026-10-26-boinc-gpu-release.md), under
[#529](https://github.com/tgoodyear/usnewsmap.com/issues/529).
Publication alone had not exercised the volunteer hardware.

## Intel: returned diagnostics, not yet accepted offload

WUs **20673/20674** targeted BOINC host 9, Intel Iris Xe, client 8.2.13,
Docker, signed Surya **1.6/106**, app-version 60. The exact worker image was
`sha256:e908ebe32270ec0bd6ae479478e0bcf5be97ff26b80d251d1afc2945b9e1259e`,
from reviewed source `614756c`; its engine pins are in the release note.

These were diagnostic quorum-one units, not independent validation: two CPUs,
8 GiB container RAM, no swap, and whole/capped modes limited to 300 seconds each.
The source bindings retained the exact public benchmark JPEGs and references.
The local-unit identity was explicitly bound to those samples, not promoted
into a full benchmark track or production OCR routing.

| Public sample                  | WU    | Result | Mode   | Outcome | Seconds | Peak process RSS, KiB | CUDA word-bag Dice |
| ------------------------------ | ----- | ------ | ------ | ------- | ------- | --------------------- | ------------------ |
| `pub-overproof-ca-044`         | 20673 | 37463  | whole  | ok      | 36.616  | 633868                | 0.969072           |
| `pub-overproof-ca-044`         | 20673 | 37463  | capped | ok      | 30.115  | 568028                | 0.969072           |
| `pub-reichsanzeiger-lines-183` | 20674 | 37464  | whole  | timeout | 300.411 | 629076                | not available      |
| `pub-reichsanzeiger-lines-183` | 20674 | 37464  | capped | timeout | 300.394 | 542940                | not available      |

Both returns passed the stock `usnm-boinc surya check --files` check in output
template order: tiled, whole, capped, manifest, diagnostic. Retained manifest
hashes matched the mode texts and diagnostic. Successful BOINC return or
quorum-one canonical status does not turn a timeout into a reading.

The English CUDA comparison used the same input page, Surya version and
weights, with a retained CUDA result whose modes reported `cuda`.
Dice is descriptive post-collection word-bag overlap, using the contract's
lowercased alphanumeric-token multiset definition. It is not a stock
cross-host validation vote: the Vulkan and CUDA engine identities differ.
The retained CPU baseline ran only tiled mode, so it supplies no same-mode
comparison here. German timeouts supply no output agreement.

All four Intel modes reported **`device=none`, no offload count**. `/dev/dri`
mapping, Vulkan discovery and `-ngl 99` do not prove model offload.
The Vulkan launch omitted `-lv 4`, already used by CPU/CUDA to expose
llama-library startup/offload evidence. The published Vulkan binary advertises
that verbosity option. [#1177](https://github.com/tgoodyear/usnewsmap.com/pull/1177)
adds it without changing device selection, inference or validation.
A corrected signed image and fresh BOINC return are still needed for
[#842](https://github.com/tgoodyear/usnewsmap.com/issues/842).

The old generic queue required 12 GiB, above this client's 9 GiB BOINC RAM
allowance. Exact-host assignment also needed an eligible app version under
ordinary plan feasibility. Only the interval **20673-20674** was temporarily
opened for Intel Surya. It was restored to **closed 2/1** after assignment;
the complete plan XML SHA-256 returned to
`98393966c4ae0dcee65bdc30df2e1fa6938fb16212f4842a81accd94e765e4f5`.
No host preferences, pauses, work-request flags, forced RPCs, ordinary validation
thresholds or production routing were changed.

## AMD: eligible new work, but no RX 9070 XT return

The existing eight unsent native-8B workunits had each already been attempted
by RX 9070 XT host 5. BOINC's one-result-per-host-per-workunit rule prevents
those retries returning to that host; an unsent count alone was not enough.

Exactly one deliberate fresh diagnostic repeat, **WU20675**, reused the four
checksum-verified inputs of independently validated public-source WU17100.
It retained native batch 8, 6 GiB RAM, 18 GiB disk, quorum two and two replicas.
It was not host-targeted and must not be imported as a new corpus reading.

At the recorded queue check, **37460** had returned successfully from **Apple
GPU host 1**, app-version 37 / version 105 / `apple_gpu_8b`, and was awaiting
independent validation. **37461** was unsent. Neither establishes AMD behavior.
Normal eligible peers may consume the remaining replica before AMD reconnects;
do not keep generating duplicates to force an AMD result.

Host 5's last RPC was **24 October 2026, 16:05:15 UTC**. This establishes lack
of client contact, not the machine's power state. Signed AMD Linux 1.9/109 and
Windows 1.10/110 are available, but
[#885](https://github.com/tgoodyear/usnewsmap.com/issues/885) still needs a
current-release RX 9070 XT result, actual Vulkan offload and diagnostics,
and independent peer validation. Existing automatic AMD admission is unchanged.
