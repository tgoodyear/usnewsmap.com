import { ALPHA_UNCLEAR, cssSkewColor, SKEW_TICKS, tickLabel } from "../lib/skewScale";
import type { BucketUnit } from "../api/types";

interface Props {
  /** Places and states with pages in the current window. */
  places: number;
  states: number;
  unit: BucketUnit;
}

const UNIT_PLURAL: Record<BucketUnit, string> = { year: "years", month: "months", week: "weeks", day: "days" };

/** Legend for the relative-rate view: a diverging ramp centred on 1×. */
export function SkewLegend({ places, states, unit }: Props) {
  return (
    <div className="legend legend--skew">
      <div className="legend__title">Relative rate</div>
      <div className="legend__ramp" aria-hidden="true">
        {SKEW_TICKS.map((x) => (
          <span key={x} style={{ background: cssSkewColor(x) }} />
        ))}
      </div>
      <div className="legend__ticks" aria-hidden="true">
        {SKEW_TICKS.map((x) => (
          <span key={x}>{tickLabel(x)}</span>
        ))}
      </div>
      <p className="legend__note">
        {places > 1 ? (
          <>
            Matches per page compared with the other {(places - 1).toLocaleString("en-US")}{" "}
            {places === 2 ? "place" : "places"} with pages in this window, in {states.toLocaleString("en-US")}{" "}
            {states === 1 ? "state" : "states"}, over the same {UNIT_PLURAL[unit]}. 1× is the same rate.
          </>
        ) : (
          "No other place has pages in this window, so there is nothing to compare with."
        )}
      </p>
      <p className="legend__note">
        <span className="legend__swatch" style={{ background: cssSkewColor(1, ALPHA_UNCLEAR) }} aria-hidden="true" />{" "}
        Faded: can't tell. Circle area: matches expected.
      </p>
    </div>
  );
}
