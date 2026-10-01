import { useEffect, useRef, useState } from "react";
import { buildSkew, type SkewModel } from "./skewModel";
import type { Prepared } from "./skewInput";

export type SkewState =
  | { status: "idle" }
  | { status: "computing" }
  | { status: "ready"; model: SkewModel; prepared: Prepared }
  | { status: "error" };

/**
 * Fit the relative-rate model for `prepared` (null: nothing to fit) in a
 * worker, or on the main thread where workers aren't available (tests).
 */
export function useSkewModel(prepared: Prepared | null): SkewState {
  const [state, setState] = useState<{ for: Prepared | null; value: SkewState }>({
    for: null,
    value: { status: "idle" },
  });
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
      if (id === seq.current) setState({ for: prepared, value });
    };
    if (typeof Worker === "undefined") {
      const timer = setTimeout(() => {
        try {
          done({ status: "ready", model: buildSkew(prepared.input), prepared });
        } catch {
          done({ status: "error" });
        }
      }, 0);
      return () => clearTimeout(timer);
    }
    if (!worker.current) {
      worker.current = new Worker(new URL("./skew.worker.ts", import.meta.url), { type: "module" });
    }
    const w = worker.current;
    const onMessage = (e: MessageEvent<{ id: number; model?: SkewModel; error?: string }>) => {
      if (e.data.id !== id) return;
      done(e.data.model ? { status: "ready", model: e.data.model, prepared } : { status: "error" });
    };
    const onError = () => done({ status: "error" });
    w.addEventListener("message", onMessage);
    w.addEventListener("error", onError);
    w.postMessage({ id, input: prepared.input });
    return () => {
      w.removeEventListener("message", onMessage);
      w.removeEventListener("error", onError);
    };
  }, [prepared]);

  if (!prepared) return { status: "idle" };
  return state.for === prepared ? state.value : { status: "computing" };
}
