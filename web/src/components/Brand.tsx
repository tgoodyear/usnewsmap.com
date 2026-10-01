/**
 * Outline of the contiguous United States (the 48 states and DC), in a
 * 32 × 20 box. Made once from us-atlas `states-10m.json` (Census
 * cartographic boundaries, public domain): states merged, islands dropped,
 * simplified to ~50 points, Albers conic projection (parallels 29.5° and
 * 45.5°, centred on 96° W).
 */
const CONTIGUOUS_US =
  "M25.5,15.2L26.3,19.3L23.7,15.9L21.7,16L21.1,16L20.4,16.2L17.7,16.7L14.2,19L12.8,16.5L9.8,14.8L8.2,14.9L4.9,13.3L1.7,11.3L0.8,7.6L1.2,5.3L2.5,2.4L2.4,0.8L6.1,1.3L6.6,1.4L12.4,2.3L15.7,2.4L19.5,3L18.3,4.1L19.1,4.1L20.7,5.1L20.7,7L20.9,7.6L21.3,7.5L21.4,5.3L22.7,4.7L23.1,7.3L24.7,6.9L25,6.6L28,3.9L28.9,3.7L29,3.4L29.5,1.7L31.2,3.2L29.7,5L29.8,6.2L29.7,6L29.4,6.4L28.5,6.9L28.3,7.1L28.3,7.3L27.7,8.1L28.2,8.9L28.1,9.2L27.9,9.3L27,9L28,10.4L26.8,12.6L25.6,14.2Z";

/** The site name and mark in every page's top bar; links home. */
export function Brand() {
  return (
    <a className="brand" href="/" aria-label="US News Map home">
      <svg className="brand__mark" viewBox="0 0 32 20" aria-hidden="true" focusable="false">
        <path d={CONTIGUOUS_US} />
      </svg>
      US News Map
    </a>
  );
}
