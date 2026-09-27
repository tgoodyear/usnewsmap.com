import { useEffect, useRef, useState } from "react";
import type { BucketUnit } from "../api/types";
import { bucketLabel, bucketStart } from "../lib/time";

interface Props {
  unit: BucketUnit;
  from: string;
  count: number;
  t: number;
  window: number | null;
  onSeek: (t: number) => void;
  onWindow: (w: number | null) => void;
}

const SPEEDS = [0.5, 1, 2, 4];
const WINDOWS = [null, 1, 3, 6, 12, 52];

function isTyping(el: EventTarget | null): boolean {
  return (
    el instanceof HTMLElement &&
    (el.isContentEditable || ["INPUT", "SELECT", "TEXTAREA"].includes(el.tagName))
  );
}

/** Playback controls (F-04 – F-06); keyboard operable (07 §7.6). */
export function TimeDock({ unit, from, count, t, window, onSeek, onWindow }: Props) {
  const [playing, setPlaying] = useState(false);
  const [speed, setSpeed] = useState(1);
  // Playback and key handlers read the latest position without re-subscribing.
  const tRef = useRef(t);
  useEffect(() => {
    tRef.current = t;
  }, [t]);

  // Playback advances ~4 buckets per second at 1×.
  useEffect(() => {
    if (!playing) return;
    let raf = 0;
    let last = performance.now();
    let acc = 0;
    const step = (now: number) => {
      acc += ((now - last) / 1000) * 4 * speed;
      last = now;
      if (acc >= 1) {
        const next = tRef.current + Math.floor(acc);
        acc -= Math.floor(acc);
        if (next >= count - 1) {
          onSeek(count - 1);
          setPlaying(false);
          return;
        }
        onSeek(next);
      }
      raf = requestAnimationFrame(step);
    };
    raf = requestAnimationFrame(step);
    return () => cancelAnimationFrame(raf);
  }, [playing, speed, count, onSeek]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (isTyping(e.target) || e.metaKey || e.ctrlKey || e.altKey) return;
      const jump = e.shiftKey ? 10 : 1;
      if (e.key === " ") {
        e.preventDefault();
        setPlaying((p) => {
          if (!p && tRef.current >= count - 1) onSeek(0);
          return !p;
        });
      } else if (e.key === "ArrowLeft") onSeek(Math.max(0, tRef.current - jump));
      else if (e.key === "ArrowRight") onSeek(Math.min(count - 1, tRef.current + jump));
      else if (e.key === "Home") onSeek(0);
      else if (e.key === "End") onSeek(count - 1);
      else return;
      e.preventDefault();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [count, onSeek]);

  const label = bucketLabel(unit, bucketStart(unit, from, t));
  return (
    <div className="dock" role="group" aria-label="Playback">
      <button
        type="button"
        className="button"
        aria-pressed={playing}
        onClick={() => {
          if (!playing && t >= count - 1) onSeek(0);
          setPlaying(!playing);
        }}
      >
        {playing ? "❚❚ Pause" : "▶ Play"}
      </button>
      <button type="button" className="button" aria-label="Previous" onClick={() => onSeek(Math.max(0, t - 1))}>
        ◀
      </button>
      <button type="button" className="button" aria-label="Next" onClick={() => onSeek(Math.min(count - 1, t + 1))}>
        ▶
      </button>
      <label>
        <span className="visually-hidden">Speed</span>
        <select value={speed} onChange={(e) => setSpeed(Number(e.target.value))}>
          {SPEEDS.map((s) => (
            <option key={s} value={s}>
              {s}×
            </option>
          ))}
        </select>
      </label>
      <label>
        <span className="visually-hidden">Window</span>
        <select
          value={window === null ? "cum" : String(window)}
          onChange={(e) => onWindow(e.target.value === "cum" ? null : Number(e.target.value))}
        >
          {WINDOWS.map((w) => (
            <option key={w ?? "cum"} value={w ?? "cum"}>
              {w === null ? "Cumulative" : `Last ${w} ${unit}${w > 1 ? "s" : ""}`}
            </option>
          ))}
        </select>
      </label>
      <input
        className="dock__scrubber"
        type="range"
        min={0}
        max={Math.max(count - 1, 0)}
        value={t}
        onChange={(e) => onSeek(Number(e.target.value))}
        aria-label="Time"
        aria-valuetext={label}
      />
      <output className="dock__label" aria-live="polite">
        {label}
      </output>
    </div>
  );
}
