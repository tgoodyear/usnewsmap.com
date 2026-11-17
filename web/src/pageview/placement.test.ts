import { describe, expect, it } from "vitest";
import type { PageLayout } from "../api/types";
import {
  clampZoom,
  confidenceHue,
  docParts,
  fontCqw,
  locViewerUrl,
  nextIndex,
  percentBox,
  placeWords,
  scaleXFor,
  wordId,
} from "./placement";

const layout: PageLayout = {
  doc_id: "loc:sn85055101_1917-04-03_ed-1_seq-3",
  engine: "usnm-tesseract",
  width: 5000,
  height: 7000,
  granularity: "word",
  image: { iiif_base: null, full: null },
  lines: [
    {
      b: 2,
      l: 1,
      rect: [500, 700, 2000, 100],
      words: [
        { i: 0, t: "Liberty", rect: [500, 700, 700, 90], c: 0.93 },
        { i: 1, t: "bell", rect: [1300, 705, 300, 95] },
      ],
    },
    { b: 2, l: 2, rect: [500, 800, 1000, 0], words: [{ i: 0, t: "rung", rect: [500, 800, 500, 80], c: null }] },
  ],
};

describe("placeWords", () => {
  it("places each word by percent of the page and keeps reading order", () => {
    const words = placeWords(layout);
    expect(words.map((w) => w.text)).toEqual(["Liberty", "bell", "rung"]);
    expect(words.map((w) => w.index)).toEqual([0, 1, 2]);
    expect(words[0]!.left).toBeCloseTo(10);
    expect(words[0]!.top).toBeCloseTo(10);
    expect(words[0]!.width).toBeCloseTo(14);
    expect(words[0]!.height).toBeCloseTo((90 / 7000) * 100);
  });

  it("sizes the font from the line's height, else the word's", () => {
    const [a, b, c] = placeWords(layout);
    expect(a!.fontCqw).toBeCloseTo(fontCqw(100, 5000));
    expect(b!.fontCqw).toBe(a!.fontCqw);
    expect(c!.fontCqw).toBeCloseTo(fontCqw(80, 5000));
  });

  it("fits each word's width with scaleX from one measurement", () => {
    const measure = (t: string) => t.length * 0.5;
    const [a] = placeWords(layout, measure);
    // Natural: 7 chars * 0.5em at fontCqw; box: 14% of the page's width.
    expect(a!.scaleX).toBeCloseTo(14 / (3.5 * fontCqw(100, 5000)));
  });

  it("keeps confidence, or null when the engine gave none", () => {
    expect(placeWords(layout).map((w) => w.conf)).toEqual([0.93, null, null]);
  });

  it("returns nothing for a page without a size", () => {
    expect(placeWords({ ...layout, width: 0 })).toEqual([]);
  });
});

describe("placement helpers", () => {
  it("percentBox", () => {
    expect(percentBox([250, 700, 500, 70], 5000, 7000)).toEqual({ left: 5, top: 10, width: 10, height: 1 });
  });

  it("scaleXFor clamps and ignores a word with no width", () => {
    expect(scaleXFor(2, 10, 20)).toBe(1);
    expect(scaleXFor(2, 10, 1)).toBe(0.3);
    expect(scaleXFor(0.1, 10, 100)).toBe(3);
    expect(scaleXFor(0, 10, 100)).toBe(1);
    expect(scaleXFor(2, 10, 0)).toBe(1);
  });

  it("wordId uses the stamp, else the engine", () => {
    expect(wordId({ engine: "e", stamp: "20261101" }, 2, 3, 4)).toBe("20261101:2:3:4");
    expect(wordId({ engine: "e" }, 0, 0, 0)).toBe("e:0:0:0");
  });

  it("confidenceHue runs red to green", () => {
    expect(confidenceHue(0)).toBe(0);
    expect(confidenceHue(0.5)).toBe(60);
    expect(confidenceHue(1)).toBe(120);
    expect(confidenceHue(7)).toBe(120);
    expect(confidenceHue(null)).toBeNull();
  });

  it("reads a loc.gov link from the document id", () => {
    const want = "https://www.loc.gov/resource/sn85055101/1917-04-03/ed-1/?sp=3";
    expect(locViewerUrl("loc:sn85055101_1917-04-03_ed-1_seq-3")).toBe(want);
    expect(locViewerUrl("sn85055101_1917-04-03_ed-1_seq-3")).toBe(want);
    expect(locViewerUrl("psu:sn85055101_1917-04-03_ed-1_seq-3")).toBeNull();
    expect(locViewerUrl("nonsense")).toBeNull();
    expect(docParts("loc:sn85055101_1917-04-03_ed-2_seq-10")).toEqual({
      lccn: "sn85055101",
      date: "1917-04-03",
      edition: 2,
      seq: 10,
    });
  });

  it("moves between words with the arrow keys and stops at the ends", () => {
    expect(nextIndex("ArrowRight", 0, 3)).toBe(1);
    expect(nextIndex("ArrowRight", 2, 3)).toBe(2);
    expect(nextIndex("ArrowLeft", 0, 3)).toBe(0);
    expect(nextIndex("Home", 2, 3)).toBe(0);
    expect(nextIndex("End", 0, 3)).toBe(2);
    expect(nextIndex("PageDown", 0, 100)).toBe(25);
    expect(nextIndex("a", 0, 3)).toBeNull();
    expect(nextIndex("ArrowRight", 0, 0)).toBeNull();
  });

  it("clampZoom", () => {
    expect(clampZoom(0.2)).toBe(1);
    expect(clampZoom(9)).toBe(4);
    expect(clampZoom(1.5)).toBe(1.5);
  });
});
