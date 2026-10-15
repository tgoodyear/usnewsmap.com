import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import type { Sources } from "../api/types";
import { catalog, creditFor, licenseHref, servingCredits } from "./data";
import { creditHref } from "../lib/ocrCredits";

export const sources: Sources = {
  american_stories: null,
  ja_latin: {
    pages: 3,
    by_reason: [],
    by_title: [],
    by_engine: [{ source: "usnm-ndlocr-lite", engine: "ndlocr-lite 636d1cf", pages: 3 }],
  },
  ocr_local: { pages: 2, by_engine: [{ source: "usnm-tesseract", engine: "tesseract/5.5.1 eng", pages: 2 }] },
};

describe("serving engine credits from engines.toml", () => {
  it("keeps catalog model versions aligned with the production image lock", () => {
    const lock = readFileSync("../ocr/engines/tesseract/models.lock", "utf8");
    for (const line of lock.split("\n").filter((l) => l && !l.startsWith("#"))) {
      const [name, url] = line.split(/\s+/);
      const version = url!.includes("tessdata_best/") ? url!.split("/")[5] : "0.905";
      const model = catalog.engine.find((e) => e.id === "tesseract-5.5")!.model!.find((m) => m.name === name)!;
      expect(model.version).toBe(version);
    }
  });

  it("requires a credit for every engine in the serving inventory", () => {
    expect(servingCredits(sources).map((c) => c.key)).toEqual(["ndlocr-lite 636d1cf", "tesseract/5.5.1 eng"]);
    expect(() =>
      servingCredits({
        ...sources,
        ocr_local: { pages: 1, by_engine: [{ source: "usnm-new", engine: "new/1 eng", pages: 1 }] },
      }),
    ).toThrow("Missing OCR credit: new/1 eng");
  });
  it("covers every production profile, not every inventoried model", () => {
    for (const engine of catalog.engine.filter((e) => e.production)) {
      for (const profile of engine.profile ?? []) {
        const credit = creditFor(`${engine.family}/${engine.id.slice(engine.family.length + 1)}.1 ${profile.id}`);
        expect(credit.models.map((m) => m.name)).toEqual(profile.models);
        expect(credit.languages).toEqual(profile.languages);
      }
    }
    expect(creditFor("tesseract/5.5.1 eng").models.map((m) => m.name)).toEqual(["tessdata/eng.traineddata"]);
    expect(creditFor("tesseract/5.5.1 frak2021").models[0]!.author).toContain("Stefan Weil");
    expect(() => creditFor("tesseract/5.5.1 german_print")).toThrow();
    expect(() => creditFor("tesseract/6.0 eng")).toThrow();
  });
  it("supports the legacy keys still published on hits", () => {
    expect(creditFor("tesseract-5.5.1/deu_latf+deu").models).toHaveLength(2);
    const ndl = creditFor("ndlocr-lite 636d1cf");
    expect(ndl.engine.author).toBe("National Diet Library");
    expect(ndl.models[0]!.license).toBe("LicenseRef-DINOv3");
    expect(licenseHref(ndl.models[0]!)).toContain("dinov3/blob/main/LICENSE.md");
    const key = "tesseract/5.5.1 deu_latf+deu";
    expect(decodeURIComponent(creditHref(key).split("#")[1]!)).toBe(key);
  });
  it("prefers frozen release credits and refuses an incomplete snapshot", () => {
    const credit = creditFor("tesseract/5.5.1 eng");
    const frozen = { ...credit, engine: { ...credit.engine, author: "Historical publisher" } };
    const source = { ...sources, ja_latin: null, ocr_credits: { schema: 1, credit_required: [], engines: [frozen] } };
    expect(servingCredits(source)[0]!.engine.author).toBe("Historical publisher");
    expect(() => servingCredits({ ...source, ocr_credits: { ...source.ocr_credits, engines: [] } })).toThrow(
      "Missing OCR credit",
    );
  });

  it("links licence terms and ignores unused engines", () => {
    const model = catalog.engine.find((e) => e.family === "tesseract")!.model!.find((m) => m.license === "none")!;
    expect(licenseHref(model)).toBeUndefined();
    expect(licenseHref(creditFor("tesseract/5.5.1 eng").engine)).toBe("https://spdx.org/licenses/Apache-2.0.html");
    expect(servingCredits({ ...sources, ja_latin: null, ocr_local: null })).toEqual([]);
  });
});
