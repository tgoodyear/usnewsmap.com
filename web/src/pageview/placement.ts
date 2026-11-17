import type { LayoutLine, LayoutRect, PageLayout } from "../api/types";

/**
 * Placement maths for the page view (#2026). Every length is a share of the page's width or
 * height, so the layer scales with its container. Font sizes use `cqw`, hundredths of the
 * layer's width, and the horizontal fit is a `scaleX` measured once, off-screen, with a canvas.
 */

/** The face words are drawn and measured in. */
export const WORD_FONT_FAMILY = 'Georgia, "Times New Roman", serif';

/** A glyph run's em is about this much taller than the ALTO box that holds it. */
export const EM_PER_BOX_HEIGHT = 0.85;

/** Words are never squeezed or stretched past these, so one bad box can't wreck a line. */
export const MIN_SCALE_X = 0.3;
export const MAX_SCALE_X = 3;

export interface PlacedWord {
  /** Position in reading order, across the whole page. */
  index: number;
  id: string;
  text: string;
  /** Percent of the page's width. */
  left: number;
  /** Percent of the page's height. */
  top: number;
  width: number;
  height: number;
  /** Font size in `cqw` (percent of the layer's width). */
  fontCqw: number;
  scaleX: number;
  /** Confidence 0 to 1, or null. */
  conf: number | null;
}

/** `{stamp}:{block}:{line}:{word}`, stable within one ALTO file (#2026). */
export function wordId(layout: Pick<PageLayout, "engine" | "stamp">, b: number, l: number, i: number): string {
  return `${layout.stamp || layout.engine}:${b}:${l}:${i}`;
}

/** A rect as percentages of the page. */
export function percentBox(rect: LayoutRect, width: number, height: number) {
  const [x, y, w, h] = rect;
  return {
    left: (x / width) * 100,
    top: (y / height) * 100,
    width: (w / width) * 100,
    height: (h / height) * 100,
  };
}

/** Font size in `cqw` for a box `heightPx` tall on a page `pageWidth` wide. */
export function fontCqw(heightPx: number, pageWidth: number): number {
  return (heightPx / pageWidth) * 100 * EM_PER_BOX_HEIGHT;
}

/**
 * The `scaleX` that makes `text`, `widthEm` wide at one em, fill a box `boxWidthPx` wide when
 * its font is `fontPx` tall. Independent of the layer's size, since both scale with it.
 */
export function scaleXFor(widthEm: number, fontPx: number, boxWidthPx: number): number {
  const natural = widthEm * fontPx;
  if (!(natural > 0) || !(boxWidthPx > 0)) return 1;
  return Math.min(MAX_SCALE_X, Math.max(MIN_SCALE_X, boxWidthPx / natural));
}

/** Width of `text` at an em of 1, from a measuring function (or a rough guess without one). */
export type Measure = (text: string) => number;

export function roughMeasure(text: string): number {
  return [...text].length * 0.5;
}

/** A canvas-backed {@link Measure}: no DOM layout, so 10,000 words cost a few milliseconds. */
export function canvasMeasure(): Measure {
  if (typeof document === "undefined") return roughMeasure;
  const ctx = document.createElement("canvas").getContext("2d");
  if (!ctx) return roughMeasure;
  const REF = 100;
  ctx.font = `${REF}px ${WORD_FONT_FAMILY}`;
  // jsdom's context has no real measureText.
  if (typeof ctx.measureText !== "function") return roughMeasure;
  return (text) => ctx.measureText(text).width / REF;
}

/** Every word of the page in reading order (lines as given, words left to right), placed. */
export function placeWords(layout: PageLayout, measure: Measure = roughMeasure): PlacedWord[] {
  const out: PlacedWord[] = [];
  const { width, height } = layout;
  if (!(width > 0) || !(height > 0)) return out;
  for (const line of layout.lines) {
    for (const word of line.words) {
      out.push(placeWord(layout, line, word, out.length, measure));
    }
  }
  return out;
}

function placeWord(
  layout: PageLayout,
  line: LayoutLine,
  word: LayoutLine["words"][number],
  index: number,
  measure: Measure,
): PlacedWord {
  const { width, height } = layout;
  const box = percentBox(word.rect, width, height);
  // The line's height keeps a line's words one size; a word box can be short ("a", "--").
  const heightPx = line.rect[3] > 0 ? line.rect[3] : word.rect[3];
  const font = fontCqw(heightPx, width);
  // The font is in percent of the layer's width and the box in percent of the page's, the same
  // unit, so the fit needs no pixel size.
  return {
    index,
    id: wordId(layout, line.b, line.l, word.i),
    text: word.t,
    ...box,
    fontCqw: font,
    scaleX: scaleXFor(measure(word.t), font, box.width),
    conf: typeof word.c === "number" ? word.c : null,
  };
}

/**
 * A hue for confidence: 0 is red, 1 is green, anything above `GOOD` is left unshaded by the
 * stylesheet. Returned in degrees for `hsl()`.
 */
export function confidenceHue(conf: number | null): number | null {
  if (conf === null || Number.isNaN(conf)) return null;
  return Math.round(Math.min(1, Math.max(0, conf)) * 120);
}

/**
 * The loc.gov viewer for a page, from its document id (`loc:sn85055101_1917-04-03_ed-1_seq-3`
 * or the bare form), or null for another source's ids.
 */
export function locViewerUrl(docId: string): string | null {
  if (docId.includes(":") && !docId.startsWith("loc:")) return null;
  const p = docParts(docId);
  return p ? `https://www.loc.gov/resource/${p.lccn}/${p.date}/ed-${p.edition}/?sp=${p.seq}` : null;
}

export interface DocParts {
  lccn: string;
  date: string;
  edition: number;
  seq: number;
}

/** The parts of a newspaper page's document id (`loc:` prefix optional), or null. */
export function docParts(docId: string): DocParts | null {
  const bare = docId.includes(":") ? docId.slice(docId.indexOf(":") + 1) : docId;
  const m = /^([a-z0-9]+)_(\d{4}-\d{2}-\d{2})_ed-(\d+)_seq-(\d+)$/i.exec(bare);
  return m ? { lccn: m[1]!, date: m[2]!, edition: Number(m[3]), seq: Number(m[4]) } : null;
}

/** The next word to focus for an arrow key, or null for a key that isn't navigation. */
export function nextIndex(key: string, current: number, count: number): number | null {
  if (count === 0) return null;
  switch (key) {
    case "ArrowRight":
    case "ArrowDown":
      return Math.min(count - 1, current + 1);
    case "ArrowLeft":
    case "ArrowUp":
      return Math.max(0, current - 1);
    case "Home":
      return 0;
    case "End":
      return count - 1;
    case "PageDown":
      return Math.min(count - 1, current + 25);
    case "PageUp":
      return Math.max(0, current - 25);
    default:
      return null;
  }
}

export const MIN_ZOOM = 1;
export const MAX_ZOOM = 4;
export const ZOOM_STEP = 0.5;

export function clampZoom(z: number): number {
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, Math.round(z * 100) / 100));
}
