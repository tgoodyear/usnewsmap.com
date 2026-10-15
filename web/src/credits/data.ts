import rawCatalog from "../../../ocr/engines/engines.toml";
import type { Sources } from "../api/types";

export interface Attribution {
  license: string;
  author: string;
  source: string;
  credit: string;
  terms?: string;
  fetched?: string;
  note?: string;
}
interface Model extends Attribution {
  name: string;
  version?: string;
}
interface Profile {
  id: string;
  models: string[];
  languages: string[];
}
interface Engine extends Attribution {
  id: string;
  family: string;
  production: boolean;
  model?: Model[];
  profile?: Profile[];
}
export interface CreditSnapshot {
  schema: number;
  credit_required: string[];
  engines: Omit<UsedCredit, "pages" | "japanese">[];
}
interface Catalog {
  schema: number;
  credit_required: string[];
  engine: Engine[];
}
export const catalog = rawCatalog as Catalog;
export interface UsedCredit {
  key: string;
  pages: number;
  japanese: boolean;
  version: string;
  engine: Engine;
  models: Model[];
  languages: string[];
}

export function licenseHref(entry: Attribution): string | undefined {
  if (entry.license === "none" || entry.license === "unknown") return undefined;
  return entry.terms ?? `https://spdx.org/licenses/${entry.license}.html`;
}

/** Refuse unknown keys/profiles instead of silently dropping a serving engine. */
export function creditFor(key: string, pages = 1, japanese = false): UsedCredit {
  const modern = /^([^/ ]+)\/([^ ]+) (.+)$/.exec(key);
  const legacyTesseract = /^tesseract-([^/]+)\/(.+)$/.exec(key);
  const legacyNdl = /^ndlocr-lite ([a-fA-F0-9]{7,40})$/.exec(key);
  const family = modern?.[1] ?? (legacyTesseract ? "tesseract" : legacyNdl ? "ndlocr-lite" : "");
  const version = modern?.[2] ?? legacyTesseract?.[1] ?? legacyNdl?.[1] ?? "";
  const profileId = modern?.[3] ?? legacyTesseract?.[2];
  // The legacy Japanese pipeline predates catalog routing. Its bundled
  // model follows the recorded NDLOCR-Lite/DINOv3 credit, not production=false.
  const engine = catalog.engine.find(
    (e) =>
      e.family === family &&
      (family === "ndlocr-lite"
        ? !!legacyNdl
        : e.id === `${family}-${version}` || e.id === `${family}-${version.split(".").slice(0, 2).join(".")}`),
  );
  const profile = engine?.profile?.find((p) => p.id === profileId);
  if (!engine || (family !== "ndlocr-lite" && !profile)) throw new Error(`Missing OCR credit: ${key}`);
  const models = profile
    ? profile.models.map((name) => {
        const model = engine.model?.find((m) => m.name === name);
        if (!model) throw new Error(`Missing model credit: ${key}: ${name}`);
        return model;
      })
    : (engine.model ?? []);
  validateAttributions(key, engine, models);
  return { key, pages, japanese, version, engine, models, languages: profile?.languages ?? ["jpn"] };
}

function validateAttributions(key: string, engine: Engine, models: Model[]) {
  if (!engine || !models?.length) throw new Error(`Incomplete OCR credit: ${key}`);
  for (const entry of [engine, ...models]) {
    if (
      !entry.author ||
      !entry.source ||
      !entry.credit ||
      !entry.license ||
      entry.license === "unknown" ||
      (entry.license === "none" && (!entry.note || !entry.fetched)) ||
      (entry.license.startsWith("LicenseRef-") && !entry.terms)
    )
      throw new Error(`Incomplete OCR credit: ${key}`);
  }
}

export function servingCredits(sources: Sources): UsedCredit[] {
  const snapshot = sources.ocr_credits;
  if (snapshot && snapshot.schema !== 1) throw new Error("Unsupported OCR credit snapshot");
  const select = (engine: string, pages: number, japanese: boolean): UsedCredit => {
    if (!snapshot) return creditFor(engine, pages, japanese);
    const credit = snapshot.engines.find((c) => c.key === engine);
    if (!credit) throw new Error(`Missing OCR credit: ${engine}`);
    validateAttributions(engine, credit.engine, credit.models);
    if (!credit.version || !credit.languages?.length) throw new Error(`Incomplete OCR credit: ${engine}`);
    return { ...credit, pages, japanese };
  };
  return [
    ...(sources.ocr_ja?.by_engine ?? sources.ja_latin?.by_engine ?? [])
      .filter((e) => e.pages > 0)
      .map((e) => select(e.engine, e.pages, true)),
    ...(sources.ocr_local?.by_engine ?? []).filter((e) => e.pages > 0).map((e) => select(e.engine, e.pages, false)),
  ];
}
