import type { Norm } from "../state/url";
import { InfoTip } from "./InfoTip";

interface Props {
  norm: Norm;
  onChange: (norm: Norm) => void;
  /** Measures the current search can't show, with why (e.g. the relative rate under a newspaper filter). */
  unavailable?: Partial<Record<Norm, string>>;
  /** Which pages the search is compared with (`baselineNote`, #237); null when unknown. */
  baseline?: string | null;
}

export const MEASURE_LABELS: Record<Norm, string> = {
  raw: "Pages",
  skew: "Relative rate",
  when: "Median date",
};

/** Pages, relative rate (doc 11, 11.6) or median date (#127). */
export function MeasureToggle({ norm, onChange, unavailable, baseline = null }: Props) {
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
      <RateInfo baseline={baseline} />
    </div>
  );
}

/** What the relative rate means, and which pages it compares with. */
function RateInfo({ baseline }: { baseline: string | null }) {
  return (
    <InfoTip label="About the relative rate">
      <p>
        Relative rate compares each place's share of matching pages with the other places' share in the same time
        periods, so places with more newspapers don't stand out just for their size. 1× is the same rate.
      </p>
      {baseline && <p>{baseline}</p>}
      <p>
        Places with few pages are pulled toward the typical rate. Each place's 90% range is where the model puts its
        rate with 90% probability. Places drawn faded could be at 1× (their 90% range includes it).
      </p>
    </InfoTip>
  );
}
