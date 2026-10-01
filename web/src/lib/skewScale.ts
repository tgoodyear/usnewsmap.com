// Diverging colour scale for the relative-rate view (doc 11, 11.6): the
// estimate on a log2 axis centred on 1×, clamped at 1/8× and 8×. Blue below,
// red above, a neutral grey at 1× (ColorBrewer RdBu arms, which stay
// distinguishable with the common colour-vision deficiencies; the midpoint
// is darker than RdBu's near-white so it shows on a light basemap).

type RGBA = [number, number, number, number];

/** Stops at log2 = -3 … 3 (1/8× … 8×). */
const STOPS: [number, number, number][] = [
  [33, 102, 172],
  [67, 147, 195],
  [146, 197, 222],
  [181, 178, 170],
  [244, 165, 130],
  [214, 96, 77],
  [178, 24, 43],
];

export const SKEW_CAP = 8;
/** Legend ticks, in times the other places' rate. */
export const SKEW_TICKS = [1 / 8, 1 / 4, 1 / 2, 1, 2, 4, 8];
/** Fill opacity (0–255): clearly above or below 1, and "can't tell". */
export const ALPHA_CLEAR = 230;
export const ALPHA_UNCLEAR = 70;

/** Position on the scale, 0 (1/8× or less) to 1 (8× or more). */
export function skewPosition(estimate: number): number {
  if (!(estimate > 0)) return 0;
  const l = Math.log2(estimate);
  return (Math.min(Math.max(l, -3), 3) + 3) / 6;
}

export function skewColor(estimate: number, alpha = ALPHA_CLEAR): RGBA {
  const v = skewPosition(estimate) * (STOPS.length - 1);
  const i = Math.min(Math.floor(v), STOPS.length - 2);
  const f = v - i;
  const a = STOPS[i]!;
  const b = STOPS[i + 1]!;
  return [
    Math.round(a[0] + (b[0] - a[0]) * f),
    Math.round(a[1] + (b[1] - a[1]) * f),
    Math.round(a[2] + (b[2] - a[2]) * f),
    alpha,
  ];
}

export function cssSkewColor(estimate: number, alpha = ALPHA_CLEAR): string {
  const [r, g, b, a] = skewColor(estimate, alpha);
  return `rgb(${r} ${g} ${b} / ${(a / 255).toFixed(2)})`;
}

/** "6.1×", "0.25×", "1/8×" style labels for rates. */
export function formatTimes(x: number): string {
  if (!Number.isFinite(x)) return "n/a";
  if (x >= 10) return `${Math.round(x)}×`;
  if (x >= 1) return `${x.toFixed(1)}×`;
  if (x >= 0.1) return `${x.toFixed(2)}×`;
  if (x >= 0.01) return `${x.toFixed(3)}×`;
  return "<0.01×";
}

/** Tick label: fractions for the left arm. */
export function tickLabel(x: number): string {
  return x < 1 ? `1/${Math.round(1 / x)}×` : `${x}×`;
}
