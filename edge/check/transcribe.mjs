// Transcription tasks for check.usnewsmap.com (#738, #1479, #968). A language whose configuration has
// `kind: "transcribe"` shows a printed line or crop and asks people to type what is printed. No machine
// reading is ever shown, so what comes back qualifies as human truth.
//
// Settling rule (README.md, "Transcription tasks"):
//   - two people (the first two, in time order) read a task blind;
//   - if their texts match exactly, the task settles as `agreed`;
//   - if they differ, a third person sees only those two human readings, picks or edits, and the task
//     settles as `adjudicated`;
//   - a reading from someone whose calibration score is bad doesn't count.
// Calibration tasks have a known text (from a human source) and are mixed in; they score the reader and
// are never exported as truth.

export const CAL_MIN = 3; // calibration tasks a person does before their readings count (when a language has any)
export const CAL_EVERY = 6; // then one calibration task per this many readings
export const CAL_MAX_CER = 0.1; // default: mean error rate on calibration above this disqualifies
export const MAX_TEXT = 4000;
export const MAX_LINES = 60;

// Bidi controls and other format or control characters are removed: the policy keeps logical order with
// none of them. Line ends become LF, trailing spaces go, and so do blank lines at either end.
export function normalizeReading(text) {
  const lines = String(text)
    .replace(/\r\n?/g, "\n")
    .replace(/[​-‏‪-‮⁠-⁩﻿]/g, "")
    // eslint-disable-next-line no-control-regex
    .replace(/[\u0000-\u0009\u000b-\u001f\u007f]/g, "")
    .split("\n")
    .map((l) => l.replace(/[ \t]+$/g, "").replace(/^[ \t]+/g, "").replace(/[ \t]{2,}/g, " "));
  while (lines.length && lines[0] === "") lines.shift();
  while (lines.length && lines[lines.length - 1] === "") lines.pop();
  return lines.join("\n");
}

// Edit distance over code points.
export function editDistance(a, b) {
  const x = Array.from(a), y = Array.from(b);
  let prev = Array.from({ length: y.length + 1 }, (_, j) => j);
  for (let i = 1; i <= x.length; i++) {
    const cur = [i];
    for (let j = 1; j <= y.length; j++) cur[j] = Math.min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (x[i - 1] === y[j - 1] ? 0 : 1));
    prev = cur;
  }
  return prev[y.length];
}

export function cer(reading, truth) {
  const t = Array.from(normalizeReading(truth)).length;
  const d = editDistance(normalizeReading(reading), normalizeReading(truth));
  return t === 0 ? (d === 0 ? 0 : 1) : d / t;
}

// readings: [{contributor, text, unreadable, at}] that count (already filtered to qualified people),
// in any order. Returns the task's new state.
export function settleReadings(readings, mode = "pair") {
  const all = [...readings].sort((p, q) => p.at - q.at || (p.contributor < q.contributor ? -1 : 1));
  const [a, b, adj] = all;
  const n = all.length;
  // A single-reader set (the maintainer reads every crop): one counted reading settles it.
  if (mode === "single") return a ? { n, settled: 1, outcome: "single", text: a.text, unreadable: a.unreadable ? 1 : 0, method: "single" } : { n, settled: 0, outcome: null };
  if (!a || !b) return { n, settled: 0, outcome: null };
  const same = a.unreadable === b.unreadable && a.text === b.text;
  if (same) return { n, settled: 1, outcome: "agreed", text: a.text, unreadable: a.unreadable ? 1 : 0, method: "agreed" };
  if (adj) return { n, settled: 1, outcome: "adjudicated", text: adj.text, unreadable: adj.unreadable ? 1 : 0, method: "adjudicated" };
  return { n, settled: 0, outcome: "adjudicate" };
}

// A person counts if the language has no calibration tasks, or they have done CAL_MIN and score well.
export function qualified(scores, hasCalibration, maxCer = CAL_MAX_CER) {
  if (!hasCalibration) return true;
  if (scores.length < CAL_MIN) return false;
  return scores.reduce((s, v) => s + v, 0) / scores.length <= maxCer;
}

// Whether the next task for a person should be a calibration task.
export function wantCalibration(calDone, readings, calAvailable) {
  if (!calAvailable) return false;
  if (calDone < CAL_MIN) return true;
  return readings >= (calDone - CAL_MIN + 1) * CAL_EVERY;
}

export function flip(id) {
  let h = 0;
  for (const c of id) h = (h * 31 + c.codePointAt(0)) >>> 0;
  return (h & 1) === 1;
}

export async function readerId(contributor) {
  const d = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode("usnm-check-reader|" + contributor)));
  return Array.from(d.slice(0, 5), (v) => v.toString(16).padStart(2, "0")).join("");
}
