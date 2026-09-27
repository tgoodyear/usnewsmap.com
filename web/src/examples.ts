import type { ViewState } from "./state/url";

export interface Example {
  id: string;
  title: string;
  blurb: string;
  view: Partial<ViewState>;
}

// Preset searches (F-30). They use phrases present in the synthetic fixtures
// and in the real corpus alike.
export const EXAMPLES: Example[] = [
  {
    id: "cross-of-gold",
    title: "Cross of Gold, 1896",
    blurb: "Watch Bryan's speech spread from Chicago to both coasts, week by week.",
    view: { q: '"cross of gold"', from: "1896-06-01", to: "1896-12-31", bucket: "week" },
  },
  {
    id: "yellow-fever",
    title: "Yellow fever",
    blurb: "Follow reports of an epidemic through the port cities.",
    view: { q: '"yellow fever"', bucket: "month", norm: "rel" },
  },
  {
    id: "free-silver",
    title: "Free silver",
    blurb: "The money question that divided the country in the 1890s.",
    view: { q: '"free silver"', bucket: "month" },
  },
];
