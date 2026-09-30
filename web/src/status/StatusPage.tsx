import { useEffect, useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, API_BASE } from "../api/client";
import type {
  Backfill,
  HourBin,
  Indexing,
  Status,
  TitlesPipeline,
} from "../api/types";
import { TITLES } from "../route";
import { count, health, relative, span, when, type Level } from "./format";

const REFRESH_MS = 30_000;

const LEVEL: Record<Level, { icon: string; label: string }> = {
  ok: { icon: "✓", label: "Working" },
  attention: { icon: "!", label: "Needs attention" },
  problem: { icon: "✕", label: "Out of date" },
  unknown: { icon: "?", label: "Partly known" },
};

/** `/status`: what the ingest pipeline is doing, for anyone (no sign-in). */
export default function StatusPage() {
  const status = useQuery({
    queryKey: ["status"],
    queryFn: ({ signal }) => api.status(signal),
    refetchInterval: REFRESH_MS,
    staleTime: 0,
  });
  // Relative times ("5 min ago") keep moving between refreshes.
  const [tick, setTick] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setTick(Date.now()), 15_000);
    return () => clearInterval(timer);
  }, []);
  const now = Math.max(tick, status.dataUpdatedAt);
  useEffect(() => {
    document.title = TITLES.status;
  }, []);

  const s = status.data;
  return (
    <div className="app status-page">
      <header className="topbar">
        <a className="brand" href="/" aria-label="US News Map home">
          <span aria-hidden="true">◉</span> US News Map
        </a>
      </header>
      <main className="status">
        <h1>Pipeline status</h1>
        {status.error && (
          <p className="notice notice--error" role="alert">
            The status could not be loaded{s ? "; showing the last copy" : ""}.
            It is retried every 30 seconds.
          </p>
        )}
        {!s && !status.error && <p role="status">Loading…</p>}
        {s && <Overview s={s} now={now} />}
        {s && <BackfillSection s={s} now={now} />}
        {s && <IndexingSection s={s} now={now} />}
        {s && <TitlesSection s={s} />}
        {s && (
          <section aria-labelledby="technical" className="status-section">
            <h2 id="technical">Technical</h2>
            <p>
              The page reads <a href={`${API_BASE}/v1/status`}>/v1/status</a>{" "}
              (JSON, schema {s.schema}). The API reads the pipeline state at
              most once a minute.
            </p>
            <details>
              <summary>Raw response</summary>
              <pre
                className="status-json"
                tabIndex={0}
                aria-label="Raw /v1/status response"
              >
                {JSON.stringify(s, null, 2)}
              </pre>
            </details>
          </section>
        )}
      </main>
      <footer className="credits">
        <a href="/">Search</a> · Newspaper pages from{" "}
        <a
          href="https://chroniclingamerica.loc.gov/"
          target="_blank"
          rel="noopener noreferrer"
        >
          Chronicling America
        </a>{" "}
        (
        <a href="https://www.loc.gov/" target="_blank" rel="noopener noreferrer">
          Library of Congress
        </a>{" "}
        and{" "}
        <a href="https://www.neh.gov/" target="_blank" rel="noopener noreferrer">
          National Endowment for the Humanities
        </a>
        ). <a href="/privacy">Privacy</a>
      </footer>
    </div>
  );
}

function Overview({ s, now }: { s: Status; now: number }) {
  const h = health(s, now);
  const level = LEVEL[h.level];
  return (
    <section aria-label="Overview" className="status-overview">
      <p className={`health health--${h.level}`}>
        <span className="health__icon" aria-hidden="true">
          {level.icon}
        </span>
        <strong>{level.label}:</strong> {h.parts.join(" · ")}
      </p>
      <p className="status-meta">
        Updated{" "}
        <time dateTime={s.generated_at}>{relative(s.generated_at, now)}</time>
        {s.pipeline.read_at && s.stale && (
          <>
            {" "}
            · <span className="badge badge--problem">Stale</span> pipeline state
            from{" "}
            <time dateTime={s.pipeline.read_at}>
              {relative(s.pipeline.read_at, now)}
            </time>
          </>
        )}{" "}
        · refreshes every 30 s
      </p>
      {s.error && (
        <p className="notice notice--error" role="note">
          The last attempt to read the pipeline state failed: {s.error}
        </p>
      )}
      {s.published.synthetic && (
        <p className="notice notice--demo" role="note">
          Demo data: this server serves a small synthetic corpus.
        </p>
      )}
    </section>
  );
}

function Unavailable({ reason }: { reason: string }) {
  return (
    <p className="notice" role="note">
      Not available. {reason}
    </p>
  );
}

function Bar({
  value,
  max,
  label,
}: {
  value: number;
  max: number;
  label: string;
}) {
  const pct = max > 0 ? Math.min(100, (100 * value) / max) : 0;
  return (
    <div
      className="progress"
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={max}
      aria-valuenow={value}
      aria-valuetext={label}
      aria-label={label}
    >
      <div className="progress__fill" style={{ width: `${pct}%` }} />
    </div>
  );
}

function Stats({ items }: { items: [string, ReactNode][] }) {
  return (
    <dl className="stats">
      {items.map(([k, v]) => (
        <div key={k}>
          <dt>{k}</dt>
          <dd>{v}</dd>
        </div>
      ))}
    </dl>
  );
}

function Table({
  caption,
  head,
  rows,
  empty,
}: {
  caption: string;
  head: string[];
  rows: ReactNode[][];
  empty: string;
}) {
  if (rows.length === 0) return <p className="status-empty">{empty}</p>;
  return (
    // Focusable so keyboard users can scroll a wide table on a narrow screen.
    <div
      className="table-scroll"
      tabIndex={0}
      role="region"
      aria-label={caption}
    >
      <table className="places status-table">
        <caption>{caption}</caption>
        <thead>
          <tr>
            {head.map((h) => (
              <th key={h} scope="col">
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((r, i) => (
            <tr key={i}>
              {r.map((c, j) => (
                <td key={j}>{c}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function Time({ iso, now }: { iso: string | null; now: number }) {
  if (!iso) return <>–</>;
  return (
    <time dateTime={iso} title={when(iso)}>
      {relative(iso, now)}
    </time>
  );
}

function BackfillSection({ s, now }: { s: Status; now: number }) {
  const b = s.backfill;
  return (
    <section aria-labelledby="backfill" className="status-section">
      <h2 id="backfill">Backfill (curating LoC batches)</h2>
      {!b.available ? (
        <Unavailable reason={b.reason} />
      ) : (
        <BackfillBody b={b} now={now} />
      )}
    </section>
  );
}

function BackfillBody({ b, now }: { b: Backfill; now: number }) {
  const t = b.throughput;
  const label = `${count(b.by_status.curated)} of ${count(b.total)} batches curated (${b.percent}%)`;
  return (
    <>
      <p className="status-lead">{label}</p>
      <Bar value={b.by_status.curated} max={b.total} label={label} />
      <Stats
        items={[
          ["Queued", count(b.by_status.queued)],
          ["In progress", count(b.in_progress)],
          ["Curated", count(b.by_status.curated)],
          ["Failed", count(b.by_status.failed)],
          ["Retrying", count(b.retrying)],
          ["Stopped workers' leases", count(b.stale_leases)],
          ["Pages curated", count(b.pages)],
          ["Pages with text", count(b.ok_pages)],
          [
            `Rate (last ${t.rate_window_hours} h)`,
            `${t.rate_per_hour} batches/h`,
          ],
          [
            "Estimated finish",
            t.eta ? (
              <Time iso={t.eta} now={now} />
            ) : t.remaining ? (
              "unknown (no recent rate)"
            ) : (
              "done"
            ),
          ],
          ["Newer versions waiting", count(b.newer_versions_pending)],
        ]}
      />
      <p>
        <strong>LoC downloads:</strong>{" "}
        {b.loc.throttled && b.loc.blocked_until ? (
          <>
            <span className="badge badge--attention">Throttled</span> LoC
            refused downloads; every worker waits until{" "}
            {when(b.loc.blocked_until)} (
            <Time iso={b.loc.blocked_until} now={now} />
            ).
          </>
        ) : b.loc.next_slot ? (
          <>
            paced across all workers (LoC allows 10 bulk downloads per 10
            minutes); next slot <Time iso={b.loc.next_slot} now={now} />.
          </>
        ) : (
          "no download is waiting for a slot."
        )}
      </p>
      <h3>Batches curated per hour, last 48 hours</h3>
      <Throughput hours={t.hours} />
      <Table
        caption="Batches in progress"
        head={["Batch", "Worker", "Claimed", "Lease ends", "Attempt"]}
        empty="No batch is being curated right now."
        rows={b.in_progress_batches.map((a) => [
          <code key="b">{`${a.batch}_ver${String(a.version).padStart(2, "0")}`}</code>,
          a.worker ?? "–",
          <Time key="s" iso={a.since} now={now} />,
          a.lease_expired ? (
            <span key="l">
              <span className="badge badge--attention">Expired</span>{" "}
              <Time iso={a.lease_until} now={now} />
            </span>
          ) : (
            <Time key="l" iso={a.lease_until} now={now} />
          ),
          a.attempts,
        ])}
      />
      <Table
        caption={`Failed batches${b.by_status.failed > b.failed_batches.length ? ` (latest ${b.failed_batches.length})` : ""}`}
        head={["Batch", "Attempts", "Last error", "When"]}
        empty="No failed batches."
        rows={b.failed_batches.map((f) => [
          <code key="b">{`${f.batch}_ver${String(f.version).padStart(2, "0")}`}</code>,
          f.attempts,
          <ErrorText key="e" text={f.error} />,
          <Time key="w" iso={f.updated_at} now={now} />,
        ])}
      />
      <Table
        caption="Recently curated"
        head={["Batch", "Pages", "Curated"]}
        empty="Nothing curated yet."
        rows={b.recent.map((r) => [
          <code key="b">{`${r.batch}_ver${String(r.version).padStart(2, "0")}`}</code>,
          count(r.pages),
          <Time key="c" iso={r.curated_at} now={now} />,
        ])}
      />
    </>
  );
}

const W = 960;
const H = 120;

/** Batches curated per hour, as bars; the table below carries the same numbers. */
function Throughput({ hours }: { hours: HourBin[] }) {
  const max = Math.max(1, ...hours.map((h) => h.batches));
  const total = hours.reduce((a, h) => a + h.batches, 0);
  const peak = hours.reduce(
    (p, h) => (h.batches > p.batches ? h : p),
    hours[0] ?? { start: "", batches: 0, pages: 0 },
  );
  const bw = W / Math.max(hours.length, 1);
  const summary =
    total === 0
      ? "No batches curated in the last 48 hours."
      : `${count(total)} batches curated in the last 48 hours; the most in one hour was ${peak.batches}, starting ${when(peak.start)}.`;
  return (
    <figure className="throughput">
      <div className="throughput__plot">
        <span className="throughput__max" aria-hidden="true">
          {max}
        </span>
        <svg
          viewBox={`0 0 ${W} ${H}`}
          preserveAspectRatio="none"
          role="img"
          aria-label={summary}
        >
          <line
            className="throughput__base"
            x1={0}
            x2={W}
            y1={H - 0.5}
            y2={H - 0.5}
          />
          {hours.map((h, i) => {
            const bh =
              h.batches > 0 ? Math.max(2, (h.batches / max) * (H - 4)) : 0;
            return (
              <rect
                key={h.start}
                className="throughput__bar"
                x={i * bw + 1}
                y={H - bh}
                width={Math.max(bw - 2, 1)}
                height={bh}
              >
                <title>{`${when(h.start)}: ${h.batches} ${h.batches === 1 ? "batch" : "batches"}, ${count(h.pages)} pages`}</title>
              </rect>
            );
          })}
        </svg>
      </div>
      <figcaption className="throughput__axis">
        <span>48 h ago</span>
        <span>{summary}</span>
        <span>now</span>
      </figcaption>
      <details>
        <summary>Hourly numbers</summary>
        <Table
          caption="Batches curated per hour"
          head={["Hour starting", "Batches", "Pages"]}
          empty="No data."
          rows={hours.map((h) => [when(h.start), h.batches, count(h.pages)])}
        />
      </details>
    </figure>
  );
}

function IndexingSection({ s, now }: { s: Status; now: number }) {
  const p = s.published;
  const i = s.indexing;
  const deltaLabel = `${p.deltas} of ${p.max_deltas} deltas used`;
  return (
    <section aria-labelledby="indexing" className="status-section">
      <h2 id="indexing">Indexing and releases</h2>
      <h3>Published version</h3>
      <Stats
        items={[
          ["Version", <code key="v">{p.index_version}</code>],
          ["Published", <Time key="p" iso={p.published_at} now={now} />],
          ["Pages", count(p.pages)],
          ["Newspapers", count(p.titles)],
          ["Places", count(p.places)],
          ["Dates", `${p.bounds.from} to ${p.bounds.to}`],
          ["Batches", p.batches === null ? "not recorded" : count(p.batches)],
        ]}
      />
      <p>
        <strong>Index set:</strong> base <code>{p.indexes[0]}</code>
        {p.deltas > 0 && (
          <>
            {" "}
            + {p.deltas} {p.deltas === 1 ? "delta" : "deltas"} (
            {p.indexes.slice(1).map((d, k) => (
              <span key={d}>
                {k > 0 && ", "}
                <code>{d}</code>
              </span>
            ))}
            )
          </>
        )}
        .
      </p>
      <p>
        {deltaLabel}.{" "}
        {p.next_release_full
          ? "The next release rebuilds everything into a new base index."
          : `Each release adds a delta; after ${p.max_deltas}, the next release rebuilds a single base index.`}
      </p>
      <Bar value={p.deltas} max={p.max_deltas} label={deltaLabel} />
      {!i.available ? (
        <Unavailable reason={i.reason} />
      ) : (
        <IndexingBody i={i} now={now} />
      )}
    </section>
  );
}

function IndexingBody({ i, now }: { i: Indexing; now: number }) {
  const r = i.release;
  return (
    <>
      <h3>Release in progress</h3>
      {r ? (
        <>
          <p className="status-lead">
            Building <code>{r.index_version}</code>: {count(r.docs_sent)} of{" "}
            {count(r.docs_expected)} pages sent ({r.percent}%),{" "}
            {count(Math.round(r.mb_sent))} MB. Updated{" "}
            <Time iso={r.updated_at} now={now} />.
          </p>
          <Bar
            value={r.docs_sent}
            max={r.docs_expected}
            label={`Release ${r.percent}% done`}
          />
        </>
      ) : (
        <p>
          {i.writer.held
            ? `The index writer is held (worker ${i.writer.holder ?? "?"}) but no build progress has been recorded yet.`
            : "No release is running."}
        </p>
      )}
      <Stats
        items={[
          [
            "Pipeline's current version",
            i.current_version ? <code key="c">{i.current_version}</code> : "–",
          ],
          [
            "Writer lock",
            i.writer.held ? (
              <span key="w">
                held by {i.writer.holder}, until{" "}
                <Time iso={i.writer.until} now={now} />
              </span>
            ) : (
              "free"
            ),
          ],
          [
            "Failed runs since the last publish",
            count(i.failed_since_last_publish ?? i.failed_runs),
          ],
          ["Failed runs (recent history)", count(i.failed_runs)],
        ]}
      />
      <Table
        caption="Recent index runs"
        head={[
          "Version",
          "Kind",
          "Status",
          "Batches",
          "Pages indexed",
          "Started",
          "Took",
          "Error",
        ]}
        empty="No index runs recorded."
        rows={i.runs.map((run) => [
          <code key="v">{run.index_version}</code>,
          run.full ? "full" : "delta",
          <RunBadge key="s" status={run.status} />,
          run.batches === null ? "–" : count(run.batches),
          run.docs ? count(run.docs) : "–",
          <Time key="t" iso={run.started_at} now={now} />,
          run.duration_secs === null ? "–" : span(run.duration_secs * 1000),
          <ErrorText key="e" text={run.error} />,
        ])}
      />
    </>
  );
}

/** An error in a table cell: short ones in full, long ones cut to a line
 * that expands on tap, so one long message can't make its row page-tall on
 * a narrow screen. */
export function ErrorText({ text }: { text: string | null | undefined }) {
  if (!text) return <>–</>;
  if (text.length <= ERROR_PREVIEW)
    return <span className="status-error">{text}</span>;
  return (
    <details className="status-error">
      <summary>{`${text.slice(0, ERROR_PREVIEW).trimEnd()}…`}</summary>
      {text}
    </details>
  );
}

const ERROR_PREVIEW = 60;

function RunBadge({ status }: { status: string }) {
  const cls =
    status === "failed"
      ? "badge--problem"
      : status === "building"
        ? "badge--attention"
        : "badge--ok";
  return <span className={`badge ${cls}`}>{status}</span>;
}

function TitlesSection({ s }: { s: Status }) {
  const t = s.titles;
  return (
    <section aria-labelledby="titles" className="status-section">
      <h2 id="titles">Titles catalog</h2>
      <Stats
        items={[
          [
            "Newspapers in the catalog",
            t.catalog.available ? count(t.catalog.titles) : "not available",
          ],
          [
            "Places in the catalog",
            t.catalog.available ? count(t.catalog.places) : "not available",
          ],
          ["Newspapers published", count(t.published_titles)],
          ["Places published", count(t.published_places)],
        ]}
      />
      {!t.pipeline.available ? (
        <Unavailable reason={t.pipeline.reason} />
      ) : (
        <TitlesBody t={t.pipeline} />
      )}
    </section>
  );
}

function TitlesBody({ t }: { t: TitlesPipeline }) {
  const opt = (x: number | null) => (x === null ? "unknown" : count(x));
  return (
    <>
      <Stats
        items={[
          ["Newspapers in curated batches", count(t.curated_titles)],
          ["Waiting for titles-sync", opt(t.awaiting_sync)],
          [
            "Batches held back for missing titles",
            opt(t.batches_waiting_for_titles),
          ],
          ["Curated batches not yet published", opt(t.unpublished_batches)],
          ["Ready for the next release", opt(t.ready_for_release)],
          [
            "Re-curated, waiting for a full release",
            opt(t.recurated_awaiting_full),
          ],
        ]}
      />
      <p className="status-note">
        A release only takes batches whose newspapers are all in the catalog;
        titles-sync adds them from LoC.
      </p>
    </>
  );
}
