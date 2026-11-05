# Holding German BOINC results until Fraktur trust is calibrated

**Status:** code merged, not deployed: the reviewed policy is [`2026-11-05-german-trust-hold.json`](2026-11-05-german-trust-hold.json), and CT 230's live `trust.json` is unchanged · **Date:** November 2026 · **Measured on:** synthetic fixtures in the `usnm-boinc` trust and pilot tests and the controller tests; CT 230's `gold/trust.json` read on 5 November (sha256 `4f0787b4…`, read-only) · **Code:** `crates/usnm-boinc/src/trust.rs`, `trust/gate.rs`, `trust/promote.rs`, `claim.rs`, `scripts/boinc/batch_scheduler.py`; [gold check](../boinc/gold-check.md#host-trust-for-usnm_ocr), [the claim](../boinc-batch-scheduler.md#the-claim) · **Issues:** #835, #1415, #1428, #968

## The question

On 1 November the maintainer decided: "German pages stay out of publishing until calibrated, using the GPU study's measured limit. OCR can run, but its output isn't trusted yet." That replaced the provisional policy of 20 October, which is still live in CT 230's `gold/trust.json` (v5): the `deu/frak2021` scope has `max_cer` 1.0 and 12 trusted hosts, so `collect --trust` publishes every German result those hosts return. CT 211's queue feeds claim `["eng", "deu"]`. The decision wasn't enforced.

The list could only say "trust these hosts below this CER". Emptying German's hosts, or setting a `max_cer` no one meets, would refuse every German result. A refused result is held back with a non-zero exit, and the controller stops refilling while any result waits to be collected. So German would stall the English feed, and the list still wouldn't say why.

## What we did

- **An explicit hold in the trust format.** A scoped policy can carry `hold`, the reason the scope is held. The list stays v5 and the field is optional, so the live list loads unchanged. A held scope must list no hosts, and the primary (top-level) scope can't be held. On a held scope `max_cer` is the limit its calibration must meet.
- **Kept, not published, and the feed carries on.** `collect --trust` checks the job's exact `(language, profile, psm, dpi)` against held scopes before the host check. A held result goes to `<root>/quarantine/<job>/` with `reason.json`, as a refused one does. The difference is that the job's record is marked `held`, `collect` prints `kept, not published: …` and exits 0, and `claim finish` answers `Held` and leaves the message alone. The controller counts a held job as settled, like a published one: it isn't collected again, isn't `collection_backlog` and doesn't stop the refill. The tick line gains `scope_held`. A record without `held` behaves as before, and the live config needs no change.
- **Lifting the hold takes a calibration within its limit.** `gold trust` (`Trust::merge`) refuses a policy for a held scope whose `max_cer` is above the hold's. Host admission (`host-trust promote`, `Trust::admit_hosts`) refuses a held scope. Re-promoting English keeps the hold. Collecting a held job again after the hold is lifted publishes it and clears `held`.
- **The reviewed policy.** [`2026-11-05-german-trust-hold.json`](2026-11-05-german-trust-hold.json) is the live list with only the German entry changed: no hosts, `max_cer` 0.179 and the hold's reason. The manifest digest stays `74fe4749…`, the 20 October entry's, because there is no German Gold challenge yet. The calibration that lifts the hold brings its own. The English entries are byte-for-byte the live ones. A test loads the file, checks it is exactly what `usnm-boinc` writes back, and checks that German is held for every host while English trusts hosts 1, 2, 8, 9, 10 and 12 and no others.

Not taken:

- **Dropping `deu` from `claim_languages`.** This stops the OCR, but the decision says OCR can run. It also enforces nothing at the publish gate: a page claimed before the change, or by a feed configured differently, would still publish.
- **Deleting a held page's queue message.** If the page is re-queued it gets a different message, and the controller stops on the conflict with the existing job.

### The measured limit

"The GPU study's measured limit" is the GPU study (#1428, from #968), Study 1. It used 1,060 human-transcribed crops: 700 Reichsanzeiger line pairs from 20 gazette pages and 360 NZZ crops from 167 pages. The study says itself that "no German trust threshold comes out of this study". The reviewed file uses 0.179 until the maintainer confirms which figure is meant:

| Candidate                                                 | CER                           | Note                                                          |
| --------------------------------------------------------- | ----------------------------- | ------------------------------------------------------------- |
| `frak2021` PSM 3, pooled (**chosen**)                     | 0.179 (limits 0.128 to 0.235) | The deployed worker's exact scope (PSM 3, 300 DPI)            |
| `frak2021` PSM 3, upper 95% limit                         | 0.235                         | Looser                                                        |
| `frak2021` PSM 3, lower 95% limit                         | 0.128                         | Stricter                                                      |
| `frak2021` PSM 3, same crops without Qwen's 9 looped ones | 0.177 (0.126 to 0.232)        | Post-hoc                                                      |
| `frak2021` PSM 3, median crop                             | 0.059                         | A median, not pooled like `max_cer`; 0.031 with long s folded |
| `frak2021` PSM 6, pooled                                  | 0.166 (0.114 to 0.225)        | Not the deployed PSM; a PSM 6 scope can't be calibrated today |

The Gold check's `max_cer` is pooled CER per host report, so a pooled figure for the exact scope fits it best. 0.179 is stricter than the upper limit, so relaxing it later is a one-line reviewed change. For comparison, the earlier ONB German run failed with held-out CER 0.0993 against a frozen limit of 0.0676.

## What we measured

| What                                                                                                                                                    | Result | How                                                     |
| ------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ | ------------------------------------------------------- |
| The live 20 October list (no `hold`) loads unchanged                                                                                                    | passes | `trust::tests`, v5 two-scope fixture                    |
| An empty, multi-line or oversized reason, a held scope with hosts, or a held primary is refused                                                         | passes | `trust::tests`                                          |
| A German result is held whatever the host, or with none; another profile or PSM is refused as before; English is unaffected                             | passes | `trust::tests`                                          |
| A calibration above the limit (1.0 or 0.18) is refused, and the file is left as it was; one at or below it lifts the hold                               | passes | `trust::tests`, `merge` and `promote_to`                |
| Host admission refuses the held scope                                                                                                                   | passes | `trust::tests`, `admit_hosts`                           |
| The reviewed file round-trips byte for byte, German is held for hosts 1 to 20, and English trusts exactly 1, 2, 8, 9, 10 and 12                         | passes | `trust::tests`, `include_bytes!`                        |
| A German job collects as `Held` and keeps its three files and reason; nothing is published; after a calibration the same job publishes and loses `held` | passes | `tests/pilot.rs`, a `ger` fixture with synthetic stores |
| `claim finish` on a held job answers `Held`, and the message stays hidden                                                                               | passes | `tests/pilot.rs`, queue fixture                         |
| The controller collects a held job once, then counts it settled and refills; it reports `scope_held`                                                    | passes | `scripts/boinc/test_batch_scheduler.py`                 |

Nothing here was measured on production, and nothing was deployed.

### Dry run against the live list

```diff
--- CT 230 /srv/usnm-boinc/project/gold/trust.json (live, sha256 4f0787b4…)
+++ docs/notes/2026-11-05-german-trust-hold.json (reviewed, sha256 53bc508e…)
@@ -19,22 +19,7 @@
   "max_cer": 0.08734328574534386,
   "policies": [
     {
-      "hosts": {
-        "sha256:247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa": [
-          1,
-          2,
-          3,
-          5,
-          6,
-          7,
-          8,
-          9,
-          10,
-          11,
-          12,
-          13
-        ]
-      },
+      "hosts": {},
       "manifest_sha256": "74fe47498a994b0624f0ee62c0bb60ad82a15e46c71a55510b5034f4c6021835",
       "scope": {
         "language": "deu",
@@ -42,7 +27,8 @@
         "psm": 3,
         "dpi": 300
       },
-      "max_cer": 1.0
+      "max_cer": 0.179,
+      "hold": "German pages stay out of publishing until calibrated (maintainer decision, 1 Nov 2026, #835). OCR runs; its output is kept in quarantine, not published."
     },
     {
       "hosts": {
```

The full sha256 of the reviewed file is `53bc508eff2f2bcbdcc824db834b5a0d968e94e15db7efb36670cbbb5f9853c5`.

## What is left

- **Confirm the limit.** The maintainer picks the figure above. If it isn't 0.179, change `max_cer` in the reviewed file and its test before the swap.
- **Deploy the code first.** The server image (`spool-out`, `gold trust` and host trust on CT 230) and the hand-off image (`collect`) both parse `trust.json`. An image without this change refuses a list with `hold`, and so fails every collect, English included. Both images must carry this change before the list is swapped. CT 211's controller picks up the scheduler change when its checkout is next updated. That is safe before the swap, because no record has `held` yet.
- **Swap the list on CT 230**, with the gold directory's lock held: back up the live file, install the reviewed one atomically, then check its digest, that the new `usnm-boinc` loads it, and that the next ticks report a rising `scope_held` with English still publishing. Restore the backup to roll back. A rollback of the images after jobs are held also needs those records collected again under the old list, because an older `usnm-boinc` doesn't know `held`. The exact commands are in the #835 comment.
- **Calibrate German** (#835): a German Gold challenge and its manifest, host reports for the exact image, and a `gold trust` run at or below the limit. Then collect the held jobs again (hand-off `collect` with `JOB_IDS`).
- **Watch the held backlog.** With `deu` claimed, held journals grow until calibration and count toward the `MAX_JOBS` caps, and `archive_history.py` doesn't move them. Drop `deu` from `claim_languages` if `scope_held` grows too far.
- **The nodes.** A held page's message comes back when its lease runs out, and an on-prem node can then read and publish the page. `trust.json` governs BOINC results only. If the decision covers node OCR of German pages too, that needs its own change.
