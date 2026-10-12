import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { AggregateResponse, HitsResponse, Meta, PlacesResponse, TitlesResponse } from "./api/types";

// The API answers from these, so App can be shown for a search (no WebGL in
// jsdom: it shows the tables, not the map).
const { api } = vi.hoisted(() => ({
  api: {
    meta: vi.fn(),
    places: vi.fn(),
    titles: vi.fn(),
    aggregate: vi.fn(),
    coverage: vi.fn(),
    days: vi.fn(),
    hits: vi.fn(),
    titlePages: vi.fn(),
  },
}));
vi.mock("./api/client", async (importOriginal) => ({ ...(await importOriginal<object>()), api }));
// jsdom has no canvas: say so quietly, as a browser without WebGL2 would.
HTMLCanvasElement.prototype.getContext = (() => null) as typeof HTMLCanvasElement.prototype.getContext;

const { App } = await import("./App");

const meta = {
  index_version: "v1",
  bounds: { from: "1836-01-01", to: "1963-12-31" },
  synthetic: false,
  titles: 1,
  limits: { max_query_chars: 256, max_slop: 20 },
  ja: null,
  congress: {
    series: [{ series: "globe", pages: 10, from: "1833-12-02", to: "1873-03-03" }],
    place: { id: "PC001", name: "Congress", lat: 38.89, lon: -77.01 },
  },
} as unknown as Meta;

const places: PlacesResponse = {
  type: "FeatureCollection",
  index_version: "v1",
  features: [
    {
      type: "Feature",
      id: "P00001",
      geometry: { type: "Point", coordinates: [-84.39, 33.75] },
      properties: { name: "Atlanta", state: "GA", precision: "city", titles: 1 },
    },
  ],
};

/** One year's answer: `hits` pages at `place`, from the newspaper `lccn` unless it is empty. */
function aggregate(place: string, hits: number, lccn: string): AggregateResponse {
  return {
    index_version: "v1",
    synthetic: false,
    query: { canonical: "gold", ast: "" },
    bucket: { unit: "year", from: "1850-01-01", to: "1850-12-31", count: 1 },
    coarsened: false,
    total: { hits, places: hits > 0 ? 1 : 0, baseline_pages: 0, papers: lccn ? 1 : 0 },
    series: { hits: [hits], baseline: null },
    places: hits > 0 ? { id: [place], hits: [hits], first_day: [54787] } : { id: [], hits: [], first_day: [] },
    cube: { p: hits > 0 ? [0] : [], b: hits > 0 ? [0] : [], h: hits > 0 ? [hits] : [], calls: 1, baseline_ref: null },
    papers: lccn
      ? { lccn: [lccn], hits: [hits], title: ["The Atlanta Intelligencer"], place_id: [place] }
      : { lccn: [], hits: [], title: [], place_id: [] },
    baseline: null,
  };
}

const titles: TitlesResponse = {
  index_version: "v1",
  titles: [
    {
      lccn: "sn82014424",
      title: "The Atlanta Intelligencer",
      place_id: "P00001",
      place: "Atlanta",
      state: "GA",
      first_year: 1849,
      last_year: 1871,
      pages: 2000,
      first_page_date: "1860-01-04",
      last_page_date: "1871-12-28",
      languages: ["eng"],
    },
    {
      lccn: "sn89053729",
      title: "The Atlanta Constitution",
      place_id: "P00001",
      place: "Atlanta",
      state: "GA",
      first_year: 1868,
      last_year: null,
      pages: 9000,
      languages: ["eng"],
    },
  ],
};

const noHits: HitsResponse = {
  index_version: "v1",
  place: null,
  title: null,
  total: 0,
  items: [],
  next_cursor: null,
} as unknown as HitsResponse;

function show(search: string) {
  window.history.replaceState(null, "", `/${search}`);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <App />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
  api.meta.mockResolvedValue(meta);
  api.places.mockResolvedValue(places);
  api.titles.mockResolvedValue(titles);
  api.hits.mockResolvedValue(noHits);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
  window.history.replaceState(null, "", "/");
});

describe("App under the Congress source (#229)", () => {
  it("lists no newspapers and says why the relative rate is off", async () => {
    api.aggregate.mockResolvedValue(aggregate("PC001", 3, ""));
    show("?q=gold&source=congress");
    const rate = await screen.findByRole("button", { name: "Relative rate" });
    expect(rate.hasAttribute("disabled")).toBe(true);
    expect(rate.getAttribute("title")).toBe("Not for Congress pages yet: they have no pages-published counts.");
    // The pages are listed at Congress's place, with no newspaper table under them.
    expect(screen.getByRole("rowheader", { name: "Congress" })).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Newspapers" })).toBeNull();
  });

  it("lists the newspapers of a newspaper search, with the relative rate on, as before", async () => {
    api.aggregate.mockResolvedValue(aggregate("P00001", 3, "sn82014424"));
    show("?q=gold");
    const rate = await screen.findByRole("button", { name: "Relative rate" });
    expect(rate.hasAttribute("disabled")).toBe(false);
    expect(screen.getByRole("region", { name: "Newspapers" })).toBeTruthy();
  });

  it("doesn't suggest removing filters a Congress search doesn't have", async () => {
    api.aggregate.mockResolvedValue(aggregate("PC001", 0, ""));
    show("?q=gold&source=congress");
    const none = await screen.findByText(/No pages match/);
    expect(none.closest(".notice")?.textContent).toBe(
      "No pages match. Try “All words” instead of an exact phrase, or widen the dates.",
    );
  });

  it("still suggests removing a newspaper search's filters", async () => {
    api.aggregate.mockResolvedValue(aggregate("P00001", 0, ""));
    show("?q=gold&lang=ger");
    const none = await screen.findByText(/No pages match/);
    expect(none.closest(".notice")?.textContent).toBe(
      "No pages match. Try “All words” instead of an exact phrase, widen the dates, or remove language or state filters.",
    );
  });
});

describe("App under a newspaper filter (07 §7.13)", () => {
  it("names the papers from the newspapers' list and says why the relative rate is off", async () => {
    api.aggregate.mockResolvedValue(aggregate("P00001", 3, "sn82014424"));
    show("?q=gold&lccn=sn82014424,sn89053729");
    const note = await screen.findByText(/Only pages from/);
    await screen.findByText("The Atlanta Constitution", { selector: "cite" });
    expect(note.textContent).toBe(
      "Only pages from The Atlanta Intelligencer and The Atlanta Constitution. Show every newspaper",
    );
    expect(api.titles).toHaveBeenCalledWith("v1", expect.anything());
    const rate = screen.getByRole("button", { name: "Relative rate" });
    expect(rate.hasAttribute("disabled")).toBe(true);
    expect(rate.getAttribute("title")).toBe("Not for chosen newspapers: pages published aren't counted per newspaper.");
  });

  it("says the map shows one paper's place only", async () => {
    api.aggregate.mockResolvedValue(aggregate("P00001", 3, "sn82014424"));
    show("?q=gold&lccn=sn82014424");
    const note = await screen.findByText(/Only pages from/);
    expect(note.textContent).toBe(
      "Only pages from The Atlanta Intelligencer, so the map shows its place only. Show every newspaper",
    );
    expect(screen.getByRole("button", { name: "Relative rate" }).getAttribute("title")).toBe(
      "Not for one newspaper: pages published aren't counted per newspaper.",
    );
  });

  it("suggests removing the newspaper filter when nothing matches", async () => {
    api.aggregate.mockResolvedValue(aggregate("P00001", 0, ""));
    show("?q=gold&lccn=sn82014424");
    const none = await screen.findByText(/No pages match/);
    expect(none.closest(".notice")?.textContent).toBe(
      "No pages match. Try “All words” instead of an exact phrase, widen the dates, or remove newspaper, language or state filters.",
    );
  });

  it("doesn't load the newspapers' list until it is wanted", async () => {
    api.aggregate.mockResolvedValue(aggregate("P00001", 3, "sn82014424"));
    show("?q=gold");
    await screen.findByRole("region", { name: "Newspapers" });
    expect(api.titles).not.toHaveBeenCalled();
  });
});

describe("browsing a newspaper's pages without a search word (#274)", () => {
  const pages: HitsResponse = {
    index_version: "v1",
    place: null,
    title: { lccn: "sn82014424", name: "The Atlanta Intelligencer" },
    total: 2000,
    days: 500,
    items: [
      {
        doc_id: "sn82014424_1860-01-04_ed-1_seq-1",
        date: "1860-01-04",
        lccn: "sn82014424",
        title: "The Atlanta Intelligencer",
        place_id: "P00001",
        edition: 1,
        seq: 1,
        front_page: true,
        snippets: ["THE ATLANTA INTELLIGENCER. Wednesday Morning …"],
        links: { viewer: "https://www.loc.gov/resource/sn82014424/1860-01-04/ed-1/?sp=1" },
      },
    ],
    next_cursor: "eyJvIjoyMH0",
  };
  const browseButton = () => screen.queryByRole("button", { name: "Browse pages of The Atlanta Intelligencer" });

  it("lists a chosen paper's pages by date when the API offers it", async () => {
    api.meta.mockResolvedValue({ ...meta, browse_pages: true });
    api.titlePages.mockResolvedValue(pages);
    api.aggregate.mockResolvedValue(aggregate("P00001", 3, "sn82014424"));
    show("?q=gold&lccn=sn82014424");
    await screen.findByText(/Only pages from/);
    fireEvent.click(screen.getByRole("button", { name: "Newspapers: The Atlanta Intelligencer" }));
    fireEvent.click(browseButton()!);
    const heading = await screen.findByRole("heading", { name: "Pages of The Atlanta Intelligencer" });
    expect(document.activeElement).toBe(heading);
    expect(api.titlePages).toHaveBeenCalledWith("sn82014424", "v1", "oldest", null, expect.anything());
    await screen.findByText("THE ATLANTA INTELLIGENCER. Wednesday Morning …");
    expect(screen.getByText(/^2,000 pages on 500 days/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Load more" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Newest first" }));
    await waitFor(() =>
      expect(api.titlePages).toHaveBeenCalledWith("sn82014424", "v1", "newest", null, expect.anything()),
    );
    // Closing puts focus back on the newspaper filter, not on the body:
    // with the close button…
    const trigger = screen.getByRole("button", { name: "Newspapers: The Atlanta Intelligencer" });
    fireEvent.click(screen.getByRole("button", { name: "Close the pages" }));
    expect(screen.queryByRole("heading", { name: /^Pages of/ })).toBeNull();
    expect(document.activeElement).toBe(trigger);
    // …and with Escape.
    fireEvent.click(trigger);
    fireEvent.click(browseButton()!);
    const again = await screen.findByRole("heading", { name: "Pages of The Atlanta Intelligencer" });
    expect(document.activeElement).toBe(again);
    fireEvent.keyDown(again, { key: "Escape" });
    expect(screen.queryByRole("heading", { name: /^Pages of/ })).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("isn't offered when the API doesn't list pages without a word", async () => {
    api.aggregate.mockResolvedValue(aggregate("P00001", 3, "sn82014424"));
    show("?q=gold&lccn=sn82014424");
    await screen.findByText(/Only pages from/);
    fireEvent.click(screen.getByRole("button", { name: "Newspapers: The Atlanta Intelligencer" }));
    await screen.findByRole("list", { name: "Chosen newspapers" });
    expect(browseButton()).toBeNull();
    expect(api.titlePages).not.toHaveBeenCalled();
  });
});
