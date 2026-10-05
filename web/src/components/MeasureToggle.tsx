import { useEffect, useId, useRef, useState } from "react";
import type { Norm } from "../state/url";

interface Props {
  norm: Norm;
  onChange: (norm: Norm) => void;
  /** Measures the current search can't show, with why (e.g. the relative rate under a newspaper filter). */
  unavailable?: Partial<Record<Norm, string>>;
}

export const MEASURE_LABELS: Record<Norm, string> = {
  raw: "Pages",
  skew: "Relative rate",
  when: "Median date",
};

/** Pages, relative rate (doc 11, 11.6) or median date (#127). */
export function MeasureToggle({ norm, onChange, unavailable }: Props) {
  const options: Norm[] = ["raw", "skew", "when"];
  return (
    <div className="measure">
      <div className="segmented" role="group" aria-label="Measure">
        {options.map((n) => (
          <button
            key={n}
            type="button"
            aria-pressed={norm === n}
            disabled={unavailable?.[n] !== undefined}
            title={unavailable?.[n]}
            onClick={() => onChange(n)}
          >
            {MEASURE_LABELS[n]}
          </button>
        ))}
      </div>
      <InfoTip />
    </div>
  );
}

/** A small disclosure that explains the relative rate. */
function InfoTip() {
  const [open, setOpen] = useState(false);
  const id = useId();
  const wrap = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: Event) => {
      if (e instanceof KeyboardEvent ? e.key === "Escape" : !wrap.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("keydown", close);
    document.addEventListener("pointerdown", close);
    return () => {
      document.removeEventListener("keydown", close);
      document.removeEventListener("pointerdown", close);
    };
  }, [open]);
  return (
    <div className="infotip" ref={wrap}>
      <button
        type="button"
        className="infotip__button"
        aria-expanded={open}
        aria-controls={id}
        aria-label="About the relative rate"
        onClick={() => setOpen((o) => !o)}
      >
        <span aria-hidden="true">i</span>
      </button>
      <div id={id} className="infotip__body" hidden={!open}>
        <p>
          Relative rate compares each place's share of matching pages with the other places' share in the same
          time periods, so places with more newspapers don't stand out just for their size. 1× is the same rate.
        </p>
        <p>
          Places with few pages are pulled toward the typical rate. Each place's 90% range is where the model puts
          its rate with 90% probability. Places drawn faded could be at 1× (their 90% range includes it).
        </p>
      </div>
    </div>
  );
}
