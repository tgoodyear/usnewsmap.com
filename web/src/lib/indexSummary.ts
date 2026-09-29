import type { Meta } from "../api/types";

/** The footer's description of the published index, e.g.
 * "5,263,258 pages from 1,094 newspapers, 1819–1963." */
export function indexSummary(
  meta: Pick<Meta, "bounds" | "pages" | "titles">,
): string {
  const n = new Intl.NumberFormat("en-US");
  const count = (k: number, one: string, many: string) =>
    `${n.format(k)} ${k === 1 ? one : many}`;
  const from = meta.bounds.from.slice(0, 4);
  const to = meta.bounds.to.slice(0, 4);
  const years = from === to ? from : `${from}–${to}`;
  const pages =
    meta.pages === undefined
      ? ""
      : `${count(meta.pages, "page", "pages")} from `;
  return `${pages}${count(meta.titles, "newspaper", "newspapers")}, ${years}.`;
}
