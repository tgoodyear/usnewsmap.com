/**
 * The pipeline's internal terms, in plain words: the one glossary the status
 * page shows (in "Technical details"), next to the raw numbers that use them.
 * docs/design/07-frontend-design.md §7.10 points here.
 */
export const TERMS: [term: string, meaning: string][] = [
  [
    "Batch",
    "How the Library of Congress delivers pages: an archive of digitized pages from one or more newspapers. Batches have versions (_ver01, _ver02) when the Library reissues them.",
  ],
  [
    "Curated",
    "Downloaded and processed (step 1): the batch's archive was fetched, and each page's text, date and newspaper were stored.",
  ],
  ["Backfill", "Downloading and processing every batch the Library lists, not only new ones."],
  [
    "Titles-sync",
    "Looking up newspaper details (step 3): each newspaper's name, place and languages, from loc.gov.",
  ],
  ["Catalog", "The newspapers whose details are looked up."],
  [
    "OCR",
    "Optical character recognition: reading the text off a page image. We run it ourselves (step 2) only on Japanese-language pages the Library ships without searchable text.",
  ],
  [
    "Release",
    "Building a new version of the search index (step 4) and publishing it (step 5).",
  ],
  [
    "Version",
    "One published state of the search index. The site searches exactly one version at a time.",
  ],
  [
    "Base and delta",
    "A full rebuild makes a base index; smaller updates add deltas on top. After 8 deltas the next update rebuilds the base.",
  ],
  ["Merge", "Combining the index's many small pieces into a few large ones, so searches open fewer files."],
  [
    "Lease",
    "A worker's claim on a batch while it processes it. An expired lease means that worker stopped; another one takes the batch.",
  ],
  ["Writer lock", "Only one index build can run at a time, and it holds this lock."],
  [
    "Rate limit",
    "loc.gov answers only so many requests a minute. When it starts refusing them, the pipeline stops asking for a while (65 minutes when looking up newspaper details).",
  ],
];
