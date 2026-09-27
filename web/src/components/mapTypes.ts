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
}
