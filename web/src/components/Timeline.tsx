import type { BucketUnit } from "../api/types";
import { bucketLabel, bucketStart } from "../lib/time";

interface Props {
  unit: BucketUnit;
  from: string;
  hits: number[];
  t: number;
  window: number | null;
  onSeek: (t: number) => void;
}

const W = 1000;
const H = 60;

/** Pages with hits per bucket. */
export function Timeline({ unit, from, hits, t, window, onSeek }: Props) {
  const values = hits;
  const max = Math.max(...values, 1e-9);
  const bw = W / Math.max(values.length, 1);
  const lo = window === null ? 0 : Math.max(0, t - window + 1);
  return (
    <svg
      className="timeline"
      viewBox={`0 0 ${W} ${H}`}
      preserveAspectRatio="none"
      role="img"
      aria-label={`Timeline of pages with hits by ${unit}`}
      onClick={(e) => {
        const r = e.currentTarget.getBoundingClientRect();
        onSeek(Math.min(values.length - 1, Math.floor(((e.clientX - r.left) / r.width) * values.length)));
      }}
    >
      {values.map((v, i) => {
        const h = v > 0 ? Math.max(1.5, (v / max) * (H - 4)) : 0;
        const active = i >= lo && i <= t;
        return (
          <rect
            key={i}
            x={i * bw + bw * 0.1}
            y={H - h}
            width={Math.max(bw * 0.8, 0.5)}
            height={h}
            className={active ? "timeline__bar timeline__bar--active" : "timeline__bar"}
          >
            <title>{`${bucketLabel(unit, bucketStart(unit, from, i))}: ${v} pages`}</title>
          </rect>
        );
      })}
      <line className="timeline__cursor" x1={(t + 0.5) * bw} x2={(t + 0.5) * bw} y1={0} y2={H} />
    </svg>
  );
}
