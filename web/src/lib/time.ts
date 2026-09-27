// Day numbers and buckets, mirroring crates/usnm-core/src/time.rs.

import type { BucketUnit } from "../api/types";

const EPOCH_MS = Date.UTC(1700, 0, 1);
const DAY_MS = 86_400_000;

/** Days since 1700-01-01 (the API's `first_day`). */
export function dayNumber(iso: string): number {
  return Math.round((Date.parse(`${iso}T00:00:00Z`) - EPOCH_MS) / DAY_MS);
}

export function dateFromDay(day: number): string {
  return new Date(EPOCH_MS + day * DAY_MS).toISOString().slice(0, 10);
}

/** First day (ISO) of bucket `i`, clamped to `from` like the server. */
export function bucketStart(unit: BucketUnit, from: string, i: number): string {
  const [y, m] = from.split("-").map(Number) as [number, number];
  let start: string;
  switch (unit) {
    case "year":
      start = `${String(y + i).padStart(4, "0")}-01-01`;
      break;
    case "month": {
      const ym = y * 12 + (m - 1) + i;
      start = `${String(Math.floor(ym / 12)).padStart(4, "0")}-${String((ym % 12) + 1).padStart(2, "0")}-01`;
      break;
    }
    case "week":
      start = dateFromDay(dayNumber(from) + i * 7);
      break;
    case "day":
      start = dateFromDay(dayNumber(from) + i);
      break;
  }
  return start < from ? from : start;
}

/** Bucket index containing `iso`, clamped to [0, count). */
export function bucketIndex(unit: BucketUnit, from: string, count: number, iso: string): number {
  const [fy, fm] = from.split("-").map(Number) as [number, number];
  const [y, m] = iso.split("-").map(Number) as [number, number];
  let i: number;
  switch (unit) {
    case "year":
      i = y - fy;
      break;
    case "month":
      i = y * 12 + m - (fy * 12 + fm);
      break;
    case "week":
      i = Math.floor((dayNumber(iso) - dayNumber(from)) / 7);
      break;
    case "day":
      i = dayNumber(iso) - dayNumber(from);
      break;
  }
  return Math.min(Math.max(i, 0), Math.max(count - 1, 0));
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** Human label for a bucket start. */
export function bucketLabel(unit: BucketUnit, iso: string): string {
  const [y, m, d] = iso.split("-").map(Number) as [number, number, number];
  const mon = MONTHS[m - 1] ?? "";
  switch (unit) {
    case "year":
      return String(y);
    case "month":
      return `${mon} ${y}`;
    case "week":
      return `Week of ${mon} ${d}, ${y}`;
    case "day":
      return `${mon} ${d}, ${y}`;
  }
}

export function formatDate(iso: string): string {
  return bucketLabel("day", iso);
}
