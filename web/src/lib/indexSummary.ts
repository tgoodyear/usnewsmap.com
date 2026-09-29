import type { Meta } from "../api/types";

/** The footer's description of the published index, e.g.
 * "2,747,479 pages from 687 newspapers. Index pages-v20260929-2." */
export function indexSummary(meta: Pick<Meta, "index_version" | "pages" | "titles">): string {
  const n = new Intl.NumberFormat("en-US");
  const size =
    meta.pages === undefined
      ? ""
      : `${n.format(meta.pages)} pages from ${n.format(meta.titles)} newspapers. `;
  return `${size}Index ${meta.index_version}.`;
}
