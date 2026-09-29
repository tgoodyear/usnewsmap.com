import type { ViewState } from "./state/url";
import examples from "./examples.json";

export interface Example {
  id: string;
  title: string;
  blurb: string;
  view: Partial<ViewState>;
  /**
   * The `/v1/aggregate` query string the app sends for `view`, without `v`.
   * The API warms exactly these searches before it serves a new index
   * version (crates/usnm-api/src/prewarm.rs); examples.test.ts checks they
   * match what the app sends.
   */
  aggregate: string;
}

// Preset searches (F-30). They use phrases present in the synthetic fixtures
// and in the real corpus alike. The API reads the same file at build time.
export const EXAMPLES: Example[] = examples as Example[];
