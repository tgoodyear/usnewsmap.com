import type { SkewInfo } from "../lib/skewText";

export interface MapPoint {
  id: string;
  name: string;
  state: string;
  precision: string;
  position: [number, number];
  /** Pages with hits in the current window. */
  value: number;
  /** value / pages published in the window. */
  rel: number;
  /** The relative rate in the current window (relative-rate view only). */
  skew?: SkewInfo;
  /**
   * The median date of the place's matching pages in the window, as a
   * position from the search's first bucket (0) to its last (1); NaN when
   * none (#127).
   */
  when?: number;
  /** That median date, labelled by its bucket ("Jul 1896"). */
  whenLabel?: string;
}
