// Sequential palette for circle fill (light → dark, CVD-safe: a single-hue
// ramp that varies mostly in lightness).

type RGBA = [number, number, number, number];

const STOPS: [number, number, number][] = [
  [253, 224, 139],
  [252, 174, 97],
  [235, 110, 70],
  [196, 55, 72],
  [120, 28, 90],
];

/** Colour for a value in [0, 1]. */
export function colorFor(x: number): RGBA {
  const v = Math.min(Math.max(Number.isFinite(x) ? x : 0, 0), 1) * (STOPS.length - 1);
  const i = Math.min(Math.floor(v), STOPS.length - 2);
  const f = v - i;
  const a = STOPS[i]!;
  const b = STOPS[i + 1]!;
  return [
    Math.round(a[0] + (b[0] - a[0]) * f),
    Math.round(a[1] + (b[1] - a[1]) * f),
    Math.round(a[2] + (b[2] - a[2]) * f),
    220,
  ];
}

// Time palette for the median-date view (#127): early dark blue through teal
// to late yellow (viridis stops), so it can't be mistaken for the pages ramp.
const TIME_STOPS: [number, number, number][] = [
  [68, 1, 84],
  [59, 82, 139],
  [33, 145, 140],
  [94, 201, 98],
  [253, 231, 37],
];

/** Colour for a position in time in [0, 1] (start to end of the search); grey when unknown. */
export function timeColor(x: number): RGBA {
  if (!Number.isFinite(x)) return [150, 150, 150, 160];
  const v = Math.min(Math.max(x, 0), 1) * (TIME_STOPS.length - 1);
  const i = Math.min(Math.floor(v), TIME_STOPS.length - 2);
  const f = v - i;
  const a = TIME_STOPS[i]!;
  const b = TIME_STOPS[i + 1]!;
  return [
    Math.round(a[0] + (b[0] - a[0]) * f),
    Math.round(a[1] + (b[1] - a[1]) * f),
    Math.round(a[2] + (b[2] - a[2]) * f),
    220,
  ];
}

export function cssTimeColor(x: number): string {
  const [r, g, b] = timeColor(x);
  return `rgb(${r} ${g} ${b})`;
}

export function cssColor(x: number): string {
  const [r, g, b] = colorFor(x);
  return `rgb(${r} ${g} ${b})`;
}

export function formatRel(x: number): string {
  if (!Number.isFinite(x)) return "—";
  if (x === 0) return "0";
  return `${(x * 100).toFixed(x >= 0.1 ? 0 : 1)}%`;
}
