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

function lockedRevision(url: string): string {
  const best =
    /^https:\/\/raw\.githubusercontent\.com\/tesseract-ocr\/tessdata_best\/([a-f0-9]{40})\/[^/]+\.traineddata$/.exec(
      url,
    );
  const frak = /^https:\/\/zenodo\.org\/records\/\d+\/files\/frak2021[-_]([0-9]+(?:\.[0-9]+)*)\.traineddata$/.exec(url);
  const revision = best?.[1] ?? frak?.[1];
  if (!revision) throw new Error(`Unrecognized locked model URL: ${url}`);
  return revision;
}

describe("serving engine credits from engines.toml", () => {
  it("keeps catalog model versions aligned with the production image lock", () => {
    const lock = readFileSync("../ocr/engines/tesseract/models.lock", "utf8");
    for (const line of lock.split("\n").filter((l) => l && !l.startsWith("#"))) {
      const [name, url] = line.split(/\s+/);
      const version = lockedRevision(url!);
      const model = catalog.engine.find((e) => e.id === "tesseract-5.5")!.model!.find((m) => m.name === name)!;
      expect(model.version).toBe(version);
    }
  });

  it("credits every newly shipped corpus-language profile", () => {
    for (const profile of ["spa", "fra", "ita", "yid", "heb", "dan", "nor", "fin", "swe", "lit", "slv", "hun", "slk"]) {
      const credit = creditFor(`tesseract/5.5.1 ${profile}`);
      expect(credit.models).toHaveLength(1);
      expect(credit.models[0]).toMatchObject({
        name: `tessdata/${profile}.traineddata`,
        license: "Apache-2.0",
        author: "Tesseract OCR project",
      });
    }
    expect(creditFor("tesseract/5.5.1 nor").languages).toEqual(["nob", "nno", "nor"]);
    expect(creditFor("tesseract/5.5.1 heb").languages).toEqual(["yid", "heb"]);
  });

  it("detects changed frak2021 revisions and rejects unknown pin formats", () => {
    expect(lockedRevision("https://zenodo.org/records/10125246/files/frak2021-0.906.traineddata")).toBe("0.906");
    expect(() => lockedRevision("https://example.com/frak2021.traineddata")).toThrow("Unrecognized locked model URL");
    expect(creditFor("tesseract/5.5.1 frak2021").models[0]!.version).toBeUndefined();
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
  it("includes Japanese-only index pages without double-counting Latin pages", () => {
    const ocr_ja = { pages: 7, by_engine: [{ source: "usnm-ndlocr-lite", engine: "ndlocr-lite 636d1cf", pages: 7 }] };
    const credits = servingCredits({ ...sources, ocr_ja });
    expect(credits.filter((c) => c.japanese)).toHaveLength(1);
    expect(credits.find((c) => c.japanese)!.pages).toBe(7);
    expect(servingCredits({ ...sources, ocr_ja, ja_latin: null, ocr_local: null })[0]!.pages).toBe(7);
    expect(() =>
      servingCredits({
        ...sources,
        ocr_ja: { pages: 1, by_engine: [{ source: "unknown", engine: "unknown/1 jpn", pages: 1 }] },
      }),
    ).toThrow("Missing OCR credit");
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
