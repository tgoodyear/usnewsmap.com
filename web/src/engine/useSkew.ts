import { useEffect, useRef, useState } from "react";
import { buildSkew, type SkewModel } from "./skewModel";
import type { Prepared } from "./skewInput";

export type Ready = { status: "ready"; model: SkewModel; prepared: Prepared };
export type SkewState = { status: "idle" } | { status: "computing" } | Ready | { status: "error" };

/**
 * Fit the relative-rate model for `prepared` (null: nothing to fit) in a
 * worker, or on the main thread where workers aren't available (tests) or
 * the worker fails. `last` is the most recent fit, kept while the next one
 * runs, so the map doesn't flip to page counts and back on every search.
 */
export function useSkewModel(prepared: Prepared | null): { state: SkewState; last: Ready | null } {
  const [state, setState] = useState<{ for: Prepared | null; value: SkewState }>({
    for: null,
    value: { status: "idle" },
  });
  const [last, setLast] = useState<Ready | null>(null);
  const worker = useRef<Worker | null>(null);
  const seq = useRef(0);

  useEffect(
    () => () => {
      worker.current?.terminate();
      worker.current = null;
    },
    [],
  );

  useEffect(() => {
    if (!prepared) return;
    const id = ++seq.current;
    const done = (value: SkewState) => {
      if (id !== seq.current) return;
      setState({ for: prepared, value });
      if (value.status === "ready") setLast(value);
    };
    let timer: ReturnType<typeof setTimeout> | undefined;
    const onMainThread = () => {
      timer = setTimeout(() => {
        try {
          done({ status: "ready", model: buildSkew(prepared.input), prepared });
        } catch {
          done({ status: "error" });
        }
      }, 0);
    };
    if (typeof Worker === "undefined") {
      onMainThread();
      return () => clearTimeout(timer);
    }
    try {
      worker.current ??= new Worker(new URL("./skew.worker.ts", import.meta.url), { type: "module" });
    } catch {
      worker.current = null;
      onMainThread();
      return () => clearTimeout(timer);
    }
    const w = worker.current;
    const onMessage = (e: MessageEvent<{ id: number; model?: SkewModel; error?: string }>) => {
      if (e.data.id !== id) return;
      done(e.data.model ? { status: "ready", model: e.data.model, prepared } : { status: "error" });
    };
    // A worker that fails to load or crashes (e.g. a stale chunk after a
    // deploy) is dropped; this fit, and later ones, run on the main thread.
    const onError = (e: Event) => {
      e.preventDefault();
      w.terminate();
      if (worker.current === w) worker.current = null;
      onMainThread();
    };
    w.addEventListener("message", onMessage);
    w.addEventListener("error", onError);
    w.postMessage({ id, input: prepared.input });
    return () => {
      clearTimeout(timer);
      w.removeEventListener("message", onMessage);
      w.removeEventListener("error", onError);
    };
  }, [prepared]);

  const current: SkewState = !prepared
    ? { status: "idle" }
    : state.for === prepared
      ? state.value
      : { status: "computing" };
  return { state: current, last };
}
