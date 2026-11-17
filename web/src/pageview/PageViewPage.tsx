import { memo, useCallback, useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import { useQuery } from "@tanstack/react-query";
import { ApiError, api, submitWordFeedback } from "../api/client";
import type { PageLayout, WordVerdict } from "../api/types";
import { Brand } from "../components/Brand";
import { formatDate } from "../lib/time";
import {
  canvasMeasure,
  clampZoom,
  confidenceHue,
  docParts,
  locViewerUrl,
  viewerFromSearch,
  MAX_ZOOM,
  MIN_ZOOM,
  nextIndex,
  placeWords,
  ZOOM_STEP,
  type PlacedWord,
} from "./placement";
import { loadMarks, saveMark } from "./marks";

const ext = { target: "_blank", rel: "noopener noreferrer" } as const;

type Mode = "text" | "overlay" | "scan";

/** `/page/<doc_id>`: one page redrawn in its scan layout (#2026, 07 §7.14). */
export default function PageViewPage({ docId }: { docId: string }) {
  const layout = useQuery({
    queryKey: ["page-layout", docId],
    queryFn: ({ signal }) => api.pageLayout(docId, signal),
  });
  const parts = docParts(docId);
  const label = parts ? `Page ${parts.seq}, ${formatDate(parts.date)}` : "Newspaper page";
  useEffect(() => {
    document.title = `${label} · US News Map`;
  }, [label]);
  // The hit's own link knows its archive (LoC, Penn State); a direct visit can only guess LoC from
  // the id, so that guess is dropped once the layout shows the scan is not LoC's.
  const given = viewerFromSearch(window.location.search);
  const scanHost = layout.data?.image?.iiif_base
    ? new URL(layout.data.image.iiif_base, "https://x.invalid").hostname
    : null;
  const viewer = given ?? (scanHost && !scanHost.endsWith("loc.gov") ? null : locViewerUrl(docId));
  return (
    <div className="app pv-app">
      <header className="topbar">
        <Brand />
      </header>
      <main className="pv-main">
        <h1>{label}</h1>
        {parts && (
          <p className="pv-sub">
            LCCN {parts.lccn} · edition {parts.edition}
            {viewer && (
              <>
                {" · "}
                <a href={viewer} {...ext}>
                  View page at the Library of Congress
                </a>
              </>
            )}
          </p>
        )}
        {layout.isPending ? (
          <p role="status" className="pv-state">
            Loading the page…
          </p>
        ) : layout.isError ? (
          layout.error instanceof ApiError && layout.error.problem.status === 404 ? (
            <section className="pv-state" aria-labelledby="pv-none">
              <h2 id="pv-none">No layout for this page yet</h2>
              <p>
                We have not stored this page's text with its positions.{" "}
                {viewer ? (
                  <>
                    You can read it at{" "}
                    <a href={viewer} {...ext}>
                      the Library of Congress
                    </a>
                    .
                  </>
                ) : (
                  <>
                    <a href="/">Search the newspapers</a> for other pages.
                  </>
                )}
              </p>
            </section>
          ) : (
            <div role="alert" className="pv-state">
              <p>The page could not be loaded. {layout.error.message}</p>
              <button type="button" onClick={() => void layout.refetch()}>
                Try again
              </button>
            </div>
          )
        ) : (
          <Viewer layout={layout.data} docId={docId} viewer={viewer} />
        )}
      </main>
    </div>
  );
}

function Viewer({ layout, docId, viewer }: { layout: PageLayout; docId: string; viewer: string | null }) {
  const scan = layout.image?.full ?? null;
  const [mode, setMode] = useState<Mode>(scan ? "overlay" : "text");
  const [conf, setConf] = useState(false);
  const [zoom, setZoom] = useState(1);
  const [popover, setPopover] = useState<{ index: number; x: number; y: number } | null>(null);
  const [scanFailed, setScanFailed] = useState(false);
  const hasScan = scan !== null && !scanFailed;
  const shown: Mode = hasScan ? mode : "text";

  // Measured once per layout, with a canvas: no DOM layout for 10,000 words.
  const words = useMemo(() => placeWords(layout, canvasMeasure()), [layout]);
  const wordsRef = useRef<HTMLDivElement>(null);
  const active = useRef(0);
  // This reader's marks on this page: a Viewer is mounted per page, so read once.
  const [marks, setMarks] = useState(() => loadMarks(docId));

  const el = (i: number) => wordsRef.current?.children[i] as HTMLButtonElement | undefined;

  const setActive = useCallback((i: number, focus: boolean) => {
    const prev = wordsRef.current?.children[active.current] as HTMLElement | undefined;
    if (prev) prev.tabIndex = -1;
    active.current = i;
    const next = wordsRef.current?.children[i] as HTMLElement | undefined;
    if (next) {
      next.tabIndex = 0;
      if (focus) next.focus();
    }
  }, []);

  // One tab stop for the whole page; arrow keys move it. Reapply the reader's marks.
  useEffect(() => {
    setActive(0, false);
  }, [words, setActive]);
  useEffect(() => {
    // A new layout's buttons are reused by index: drop the last layout's marks first.
    wordsRef.current?.querySelectorAll("[data-verdict]").forEach((n) => n.removeAttribute("data-verdict"));
    const known = new Map(words.map((w) => [w.id, w.index]));
    for (const [id, verdict] of Object.entries(marks)) {
      const i = known.get(id);
      if (i !== undefined) {
        const node = wordsRef.current?.children[i] as HTMLElement | undefined;
        node?.setAttribute("data-verdict", verdict);
      }
    }
  }, [marks, words]);

  const indexOf = (target: EventTarget | null): number | null => {
    const node = (target as HTMLElement | null)?.closest?.("[data-i]") as HTMLElement | null;
    return node ? Number(node.dataset.i) : null;
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    const i = indexOf(e.target);
    if (i === null || e.altKey || e.ctrlKey || e.metaKey) return;
    const next = nextIndex(e.key, i, words.length);
    if (next !== null) {
      e.preventDefault();
      setActive(next, true);
      el(next)?.scrollIntoView({ block: "nearest", inline: "nearest" });
    }
  };

  const onFocus = (e: React.FocusEvent) => {
    const i = indexOf(e.target);
    if (i !== null && i !== active.current) setActive(i, false);
  };

  const onClick = (e: React.MouseEvent) => {
    const i = indexOf(e.target);
    if (i === null) return;
    const box = el(i)?.getBoundingClientRect();
    if (box) setPopover({ index: i, x: box.left, y: box.bottom });
  };

  const choose = (verdict: WordVerdict) => {
    if (!popover) return;
    const word = words[popover.index]!;
    setMarks((m) => ({ ...m, [word.id]: verdict }));
    saveMark(docId, word.id, verdict);
    void submitWordFeedback(docId, word.id, verdict);
    closePopover();
  };

  const closePopover = () => {
    const i = popover?.index;
    setPopover(null);
    if (i !== undefined) el(i)?.focus();
  };

  const marked = Object.keys(marks).length;
  return (
    <>
      <div className="pv-toolbar">
        <fieldset className="pv-modes">
          <legend>Show</legend>
          <label>
            <input type="radio" name="pv-mode" checked={shown === "text"} onChange={() => setMode("text")} /> Text
          </label>
          {hasScan && (
            <>
              <label>
                <input type="radio" name="pv-mode" checked={shown === "overlay"} onChange={() => setMode("overlay")} />{" "}
                Text over scan
              </label>
              <label>
                <input type="radio" name="pv-mode" checked={shown === "scan"} onChange={() => setMode("scan")} /> Scan
              </label>
            </>
          )}
        </fieldset>
        <label className="pv-check">
          <input type="checkbox" checked={conf} onChange={(e) => setConf(e.target.checked)} /> Colour by confidence
        </label>
        <div className="pv-zoom" role="group" aria-label="Zoom">
          <button
            type="button"
            aria-label="Zoom out"
            disabled={zoom <= MIN_ZOOM}
            onClick={() => setZoom((z) => clampZoom(z - ZOOM_STEP))}
          >
            −
          </button>
          <output aria-live="polite">{Math.round(zoom * 100)}%</output>
          <button
            type="button"
            aria-label="Zoom in"
            disabled={zoom >= MAX_ZOOM}
            onClick={() => setZoom((z) => clampZoom(z + ZOOM_STEP))}
          >
            +
          </button>
          <button type="button" disabled={zoom === 1} onClick={() => setZoom(1)}>
            Reset
          </button>
        </div>
      </div>
      {conf && (
        <p className="pv-legend">
          Words are coloured from red (the engine was unsure) to green (the engine was sure). Words with no confidence
          keep their colour.
        </p>
      )}
      <p className="pv-hint">
        {words.length.toLocaleString("en-US")} words read by {layout.engine}. Tab to the page, move between words with
        the arrow keys, and press Enter on a word to mark it right or wrong.
        {marked > 0 && ` You have marked ${marked} on this page.`}
        {viewer && (
          <>
            {" "}
            <a href={viewer} {...ext}>
              Open at loc.gov
            </a>
            .
          </>
        )}
      </p>
      <div className="pv-scroll" tabIndex={0} role="region" aria-label="Page">
        <div className={`pv-sizer pv--${shown}${conf ? " pv--conf" : ""}`} style={sizerStyle(layout, zoom)}>
          <div
            className="pv-layer"
            style={{ width: `${100 / zoom}%`, height: `${100 / zoom}%`, transform: `scale(${zoom})` }}
          >
            {hasScan && shown !== "text" && (
              // Decorative: the words are the text alternative.
              <img className="pv-scan" src={scan} alt="" decoding="async" onError={() => setScanFailed(true)} />
            )}
            {/* The words are in reading order; a roving tabindex keeps them to one tab stop. */}
            <div
              ref={wordsRef}
              className="pv-words"
              role="group"
              aria-label="Words on the page"
              onKeyDown={onKeyDown}
              onFocus={onFocus}
              onClick={onClick}
            >
              <Words words={words} />
            </div>
          </div>
        </div>
      </div>
      {popover && (
        <Popover
          word={words[popover.index]!}
          x={popover.x}
          y={popover.y}
          previous={marks[words[popover.index]!.id]}
          onChoose={choose}
          onClose={closePopover}
        />
      )}
    </>
  );
}

function sizerStyle(layout: PageLayout, zoom: number): CSSProperties {
  return { width: `${zoom * 100}%`, aspectRatio: `${layout.width} / ${layout.height}` };
}

/** Rendered once per page: toggles, zoom and focus never re-render the words. */
const Words = memo(function Words({ words }: { words: PlacedWord[] }) {
  return (
    <>
      {words.map((w) => {
        const hue = confidenceHue(w.conf);
        const style = {
          left: `${w.left.toFixed(3)}%`,
          top: `${w.top.toFixed(3)}%`,
          height: `${w.height.toFixed(3)}%`,
          fontSize: `${w.fontCqw.toFixed(3)}cqw`,
          transform: `scaleX(${w.scaleX.toFixed(3)})`,
          ...(hue === null ? {} : { "--pv-h": hue }),
        } as CSSProperties;
        return (
          <button key={w.index} type="button" className="pv-word" data-i={w.index} tabIndex={-1} style={style}>
            {w.text}
          </button>
        );
      })}
    </>
  );
});

function Popover({
  word,
  x,
  y,
  previous,
  onChoose,
  onClose,
}: {
  word: PlacedWord;
  x: number;
  y: number;
  previous: WordVerdict | undefined;
  onChoose: (v: WordVerdict) => void;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    ref.current?.querySelector("button")?.focus();
  }, [word.index]);
  useEffect(() => {
    const away = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node) && !(e.target as HTMLElement).closest?.(".pv-word")) onClose();
    };
    document.addEventListener("pointerdown", away);
    return () => document.removeEventListener("pointerdown", away);
  });
  const left = Math.max(8, Math.min(x, window.innerWidth - 232));
  const top = Math.max(8, Math.min(y + 6, window.innerHeight - 140));
  return (
    <div
      ref={ref}
      className="pv-popover"
      role="dialog"
      aria-label={`Mark the word ${word.text}`}
      style={{ left, top }}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          onClose();
        }
      }}
    >
      <p>
        Is “<strong>{word.text}</strong>” read correctly?
      </p>
      <div className="pv-popover__actions">
        <button type="button" aria-pressed={previous === "right"} onClick={() => onChoose("right")}>
          Right
        </button>
        <button type="button" aria-pressed={previous === "wrong"} onClick={() => onChoose("wrong")}>
          Wrong
        </button>
        <button type="button" className="link-button" onClick={onClose}>
          Close
        </button>
      </div>
    </div>
  );
}
