import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import DatasetsPage, { ErrorText } from "./StatusPage";

const unavailable = {
  available: false,
  reason: "This server is not connected to the pipeline state.",
};

const body = {
  schema: 1,
  generated_at: new Date().toISOString(),
  stale: false,
  error: null,
  pipeline: { available: false, read_at: null, reason: unavailable.reason },
  published: {
    index_version: "fixture-v1",
    published_at: "2026-09-27T00:00:00Z",
    synthetic: true,
    pages: 1872,
    titles: 6,
    places: 6,
    bounds: { from: "1895-01-01", to: "1897-12-31" },
    indexes: ["pages-base-fixture", "pages-delta-fixture-1"],
    deltas: 1,
    max_deltas: 8,
    next_release_full: false,
    batches: null,
  },
  backfill: unavailable,
  indexing: unavailable,
  titles: {
    catalog: { available: false, reason: "none" },
    published_titles: 6,
    published_places: 6,
    pipeline: unavailable,
  },
};

const noVersions = {
  schema: 1,
  generated_at: new Date().toISOString(),
  available: false,
  reason: "This server is not connected to the pipeline state.",
  read_at: null,
  serving: "fixture-v1",
  versions: [],
};

/** The indexing section, as a pipeline with `runs` reports it. */
function indexing(i: Record<string, unknown>) {
  return {
    available: true,
    current_version: "fixture-v1",
    writer: { held: false, holder: null, until: null },
    release: null,
    last_published_at: "2026-09-27T00:00:00Z",
    failed_runs: 0,
    failed_since_last_publish: 0,
    runs: [],
    ...i,
  };
}

/** A full rebuild's run in `indexing.runs`. */
function indexRun(r: Record<string, unknown>) {
  return {
    index_version: "fixture-v1",
    full: true,
    status: "published",
    new_index: "pages-base-fixture",
    indexes: 1,
    batches: 3,
    docs: 1872,
    pages: 1872,
    started_at: "2026-09-26T00:00:00Z",
    published_at: "2026-09-27T00:00:00Z",
    duration_secs: null,
    previous_version: null,
    error: null,
    ...r,
  };
}

/** `/v1/meta` for the fixture version: one searched field per text, title languages. */
const fixtureMeta = { index_version: "fixture-v1", text_layout: 1, language_filter: "title" };

/** Answers `/v1/status` with `status`, `/v1/versions` with `versions` and `/v1/meta` with `meta`. */
function serve(status: unknown, versions: unknown = noVersions, meta: unknown = fixtureMeta) {
  vi.stubGlobal(
    "fetch",
    vi.fn(
      async (url: string) =>
        new Response(
          JSON.stringify(
            url.includes("/work_summary.php")
              ? { schema: 1, updated_at: new Date().toISOString(), apps: [] }
              : url.startsWith("/v1/versions")
                ? versions
                : url.startsWith("/v1/meta")
                  ? meta
                  : status,
          ),
          {
            headers: { "content-type": "application/json" },
          },
        ),
    ),
  );
}

function renderPage() {
  return render(
    <QueryClientProvider client={new QueryClient()}>
      <DatasetsPage />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("DatasetsPage", () => {
  it("shows the published version and says which sections aren't available", async () => {
    serve(body);
    renderPage();
    expect(await screen.findByRole("heading", { level: 1, name: "Dataset details" })).toBeTruthy();
    expect(await screen.findByText("1 of 8 deltas used", { exact: false })).toBeTruthy();
    // The four pipeline sections in the technical details, and the versions.
    expect(await screen.findAllByText(/^Not available\./)).toHaveLength(5);
    expect(screen.getByRole("link", { name: "/v1/status" }).getAttribute("href")).toBe("/v1/status");
    const urls = vi.mocked(fetch).mock.calls.map((c) => c[0]);
    expect(urls).toContain("/v1/status");
    expect(urls).toContain("/v1/versions");
    expect(urls).toContain("/v1/meta");
  });

  it("answers what's in the index first and keeps the machinery for later", async () => {
    serve(body);
    renderPage();
    await screen.findByRole("heading", { level: 2, name: "What's in the index" });
    const h2 = screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent);
    expect(h2).toEqual([
      "What's in the index",
      "The datasets",
      "Pages we read ourselves",
      "Volunteer computing with BOINC",
      "Languages",
      "Places",
      "In progress",
      "Versions",
      "Technical details",
    ]);
    const datasets = screen.getAllByRole("heading", { level: 3 }).map((h) => h.textContent);
    for (const name of [
      "Chronicling America",
      "American Stories",
      "Our OCR of the Japanese-language pages",
      "More to come",
    ]) {
      expect(datasets).toContain(name);
    }
    // Every count comes from the document.
    expect(
      screen.getByText(
        /^The site covers 1,872 newspaper pages from 6 newspapers published in 6 places, dated Jan 1, 1895 to Dec 31, 1897\.$/,
      ),
    ).toBeTruthy();
    // An older API without the new fields: the page says it doesn't know.
    expect(screen.getByText("This server doesn't say how many pages that is.")).toBeTruthy();
    expect(screen.getByText("No Japanese OCR is reported for this search index.")).toBeTruthy();
    // No internal terms before the pipeline sections.
    const main = screen.getByRole("main");
    const inProgress = screen.getByRole("heading", { level: 2, name: "In progress" });
    let text = "";
    for (const el of Array.from(main.children)) {
      if (el.contains(inProgress)) break;
      text += ` ${el.textContent ?? ""}`;
    }
    expect(text).not.toMatch(/\b(batch|batches|split|splits|overlay|delta|deltas|curated)\b/i);
  });

  it("shows each dataset's pages and our own OCR from the API's numbers", async () => {
    const doc = {
      ...body,
      published: {
        ...body.published,
        by_year: [
          { year: 1895, pages: 600 },
          { year: 1896, pages: 624 },
          { year: 1907, pages: 648 },
        ],
        ja: { indexes: ["pages-ja-1"], fold: 1, pages: 10_305 },
        sources: {
          american_stories: { searched: true, pages: 1500, only_pages: 40, years: 3 },
          ja_latin: {
            pages: 5647,
            by_reason: [
              { reason: "missing", pages: 5000 },
              { reason: "mixed", pages: 647 },
            ],
            by_title: [{ lccn: "sn85001234", name: "Topaz Times", pages: 5647 }],
          },
          ocr_local: { pages: 12, by_engine: [{ source: "usnm-tesseract", engine: "tesseract/5.5.1 eng", pages: 12 }] },
        },
      },
      ocr_ja: {
        available: true,
        running: false,
        targets: { pages: 12_787, issues: 1500 },
        done: { pages: 12_768, issues: 1490 },
        undigitized: { pages: 24, issues: 16 },
        percent: 99.8,
        engine: null,
        started_at: null,
        updated_at: new Date(Date.now() - 3_600_000).toISOString(),
        eta: null,
      },
    };
    serve(doc);
    renderPage();
    await screen.findByRole("heading", { level: 3, name: "American Stories" });
    expect(screen.getByText(/^The site covers .* with pages from 3 different years\.$/)).toBeTruthy();
    // Pages by year, summed by decade.
    const decades = screen.getByRole("table", { name: "Pages by decade" });
    expect(
      within(decades)
        .getAllByRole("row")
        .map((r) => r.textContent),
    ).toEqual(["DecadePages", "1890s1,224", "1900s648"]);
    expect(screen.getByRole("img", { name: /most are from the 1890s, 1,224 pages/ })).toBeTruthy();
    expect(
      screen.getByText(
        /^1,500 pages have its text, 40 pages of them with no Library of Congress text, searchable only through American Stories \(3 years of its text read\)$/,
      ),
    ).toBeTruthy();
    expect(
      screen.getByText(
        "10,305 Japanese pages in an index of their own; the English and other Latin-script text we read on 5,647 pages in the main search, 647 pages of them where the Library of Congress text was partly readable",
      ),
    ).toBeTruthy();
    expect(
      screen.getByText(/^The first pass read 12,768 of 12,787 digitized Japanese pages.*No scan exists for 24 more\.$/),
    ).toBeTruthy();
    expect(screen.getByText(/^5,647 pages of 1 newspaper are searched with the Latin-script text/)).toBeTruthy();
    const reasons = screen.getByRole("table", { name: "Pages we read, by reason" });
    expect(within(reasons).getByText("No text from the Library of Congress")).toBeTruthy();
    expect(within(reasons).getByText("The Library of Congress text is partly readable")).toBeTruthy();
    expect(screen.getByRole("table", { name: "Pages we read, by newspaper" }).textContent).toContain("Topaz Times");
    expect(screen.getByText(/^On 12 pages our reading replaced the Library of Congress's/)).toBeTruthy();
  });

  it("says when the server leaves American Stories out, or the version has none", async () => {
    for (const [as, text] of [
      [
        { searched: false, pages: null, only_pages: null, years: null },
        "This server has its text switched off, so searches read the Library of Congress text alone.",
      ],
      [null, "Not yet: the search index the site uses now doesn't have its text."],
    ] as const) {
      serve({
        ...body,
        published: {
          ...body.published,
          sources: { american_stories: as, ja_latin: null, ocr_local: null },
        },
      });
      renderPage();
      expect(await screen.findByText(text)).toBeTruthy();
      expect(
        screen.getByText("The search index the site uses now has none of our own text in place of theirs."),
      ).toBeTruthy();
      cleanup();
    }
  });

  it("doesn't call pages searchable through American Stories when its text is switched off", async () => {
    serve({
      ...body,
      published: {
        ...body.published,
        sources: {
          american_stories: { searched: false, pages: 1500, only_pages: 40, years: 3 },
          ja_latin: null,
          ocr_local: null,
        },
      },
    });
    renderPage();
    expect(
      await screen.findByText(
        /^1,500 pages have its text, 40 pages of them with no Library of Congress text \(3 years of its text read\)$/,
      ),
    ).toBeTruthy();
    expect(screen.queryByText(/searchable only through American Stories/)).toBeNull();
  });

  it("lists the published versions and what each changed", async () => {
    const run = (version: string, day: number, pages: number, features: Record<string, number | null>) => ({
      index_version: version,
      serving: false,
      full: false,
      status: "published",
      indexes: [],
      new_index: version,
      batches: null,
      docs: 0,
      pages,
      duplicate_pages: 0,
      started_at: `2026-10-0${day}T10:00:00Z`,
      published_at: `2026-10-0${day}T12:00:00Z`,
      previous_version: null,
      build: { features },
      build_source: "recorded",
    });
    const versions = {
      ...noVersions,
      available: true,
      read_at: new Date().toISOString(),
      serving: "pages-v3",
      versions: [
        {
          ...run("pages-v3", 8, 1300, { common_grams: 1, american_stories: 1 }),
          serving: true,
          full: true,
          previous_version: "pages-v1",
        },
        { ...run("pages-v2", 6, 1000, { common_grams: 1 }), status: "failed", published_at: null },
        run("pages-v1", 5, 1000, { common_grams: 1 }),
      ],
    };
    serve(body, versions);
    renderPage();
    const table = await screen.findByRole("table", { name: "Published versions, newest first" });
    // When the version's build finished: a staged rebuild is marked published
    // then, and the site switches to it hours later.
    expect(
      within(table)
        .getAllByRole("columnheader")
        .map((h) => h.textContent),
    ).toEqual(["Version", "Built", "Kind", "Pages", "Change in pages", "What changed"]);
    const rows = within(table).getAllByRole("row").slice(1);
    // The failed run isn't a version anyone searched.
    expect(rows).toHaveLength(2);
    expect(rows[0]!.textContent).toContain("pages-v3 (the site searches this one)");
    expect(rows[0]!.textContent).toContain("Full rebuild");
    expect(rows[0]!.textContent).toContain("+300");
    expect(rows[0]!.textContent).toContain("Added American Stories' text");
    expect(rows[1]!.textContent).toContain("Update");
    expect(
      screen.getByText(
        /^The last full rebuild, pages-v3, was built on Oct 8, 2026: 1,300 pages, adding 300 pages\. Added American Stories' text\.$/,
      ),
    ).toBeTruthy();
  });

  it("shows the pages tables when the API sends them, and leaves them out when it doesn't", async () => {
    const withTables = {
      ...body,
      published: {
        ...body.published,
        by_state: [
          {
            state: "IL",
            name: "Illinois",
            places: 1,
            titles: 1,
            pages: 1872,
            percent: 100,
          },
        ],
        by_language: {
          pages_known: true,
          multilingual_titles: 0,
          multilingual_pages: 0,
          rows: [
            {
              code: "eng",
              name: "English",
              titles: 6,
              pages: 1872,
              percent: 100,
            },
          ],
        },
      },
    };
    for (const [doc, shown] of [
      [withTables, true],
      [body, false],
    ] as const) {
      serve(doc);
      renderPage();
      await screen.findByRole("heading", { level: 2, name: "Places" });
      for (const name of ["Pages by state", "Pages by language"]) {
        expect(screen.queryByRole("heading", { level: 3, name }) !== null).toBe(shown);
      }
      cleanup();
    }
  });

  it("says what is happening now, then the four steps as a list", async () => {
    const live = {
      ...body,
      activity: {
        available: true,
        now: "titles",
        source: "job",
        since: new Date(Date.now() - 3_600_000).toISOString(),
        run_started_at: null,
        run: "1b2c3d",
        reported_at: new Date().toISOString(),
        done: 342,
        total: 3464,
        percent: 9.9,
        eta: null,
        paused_until: new Date(Date.now() + 2_700_000).toISOString(),
        index_version: null,
        merge: null,
        last: null,
        next_run: null,
      },
    };
    serve(live);
    const { container } = renderPage();
    expect(await screen.findByRole("heading", { level: 3, name: "Right now" })).toBeTruthy();
    expect(
      screen.getByText(
        /^Looking up newspaper details from the Library of Congress: 342 of 3,464 done \(9\.9%\)\. Paused until (?:[A-Z][a-z]{2} \d{1,2} at )?\d{1,2}:\d\d [AP]M \S+ because loc\.gov asked us to slow down\.$/,
      ),
    ).toBeTruthy();
    expect(screen.getByRole("progressbar", { name: "342 of 3,464 newspapers looked up" })).toBeTruthy();
    const list = container.querySelector("ol.steps")!;
    const items = Array.from(list.children);
    expect(items).toHaveLength(4);
    // The current step is marked for assistive technology and says its state in words.
    const current = items.filter((li) => li.getAttribute("aria-current") === "step");
    expect(current).toHaveLength(1);
    expect(current[0]!.textContent).toContain("Step 2: Newspaper details looked up");
    expect(current[0]!.textContent).toContain("Paused");
    // Operator details are collapsed.
    const technical = screen.getByText("Show the pipeline's own numbers and terms").closest("details")!;
    expect(technical.hasAttribute("open")).toBe(false);
  });

  it("says how American Stories is searched by the served version's text layout", async () => {
    const doc = {
      ...body,
      published: {
        ...body.published,
        sources: {
          american_stories: { searched: true, pages: 1500, only_pages: 40, years: 3 },
          ja_latin: null,
          ocr_local: null,
        },
      },
    };
    const badge =
      'When a page matched only in American Stories\' text, the result says so with an "American Stories OCR" badge';
    const match = "a page when its Library of Congress and American Stories texts together have the words";
    for (const [meta, text] of [
      // One field for the texts: always searched, and no count of the pages only it matched.
      [{ ...fixtureMeta, text_layout: 2 }, `Every search reads both texts: it finds ${match}. ${badge}.`],
      [{ ...fixtureMeta, text_layout: 3 }, `Every search reads both texts: it finds ${match}. ${badge}.`],
      [fixtureMeta, `A search finds ${match}. ${badge}, and the search's page count says how many pages that is.`],
      // /v1/meta for another version says nothing about this one.
      [{ ...fixtureMeta, index_version: "fixture-v2", text_layout: 1 }, `A search finds ${match}. ${badge}.`],
    ] as const) {
      serve(doc, noVersions, meta);
      renderPage();
      expect(await screen.findByText(text)).toBeTruthy();
      expect(
        screen.getByText(
          "Newspaper scans come from Chronicling America. Search combines the Library of Congress text, American Stories' text, and selected replacement OCR described below.",
        ),
      ).toBeTruthy();
      cleanup();
    }
  });

  it("says that layout 3 searches our OCR's text beside the Library of Congress's, whatever the setting", async () => {
    const doc = {
      ...body,
      published: {
        ...body.published,
        sources: {
          american_stories: { searched: false, pages: 1500, only_pages: 40, years: 3 },
          ja_latin: null,
          ocr_local: null,
        },
      },
    };
    serve(doc, noVersions, { ...fixtureMeta, text_layout: 3 });
    renderPage();
    // The American Stories setting is ignored on a one-field layout, so its text is not "switched off".
    expect(await screen.findByText(/^Every search reads both texts:/)).toBeTruthy();
    expect(screen.queryByText(/switched off/)).toBeNull();
    expect(
      screen.getByText(
        "Every search reads the Library of Congress's text, and on the pages we read again ourselves (below) our text too. Each result links to the page image at the Library of Congress.",
      ),
    ).toBeTruthy();
    cleanup();
    // Before layout 3, our text is searched in its place.
    serve(doc, noVersions, { ...fixtureMeta, text_layout: 2 });
    renderPage();
    expect(
      await screen.findByText(
        "Every search reads the Library of Congress's text, except on the pages we read again ourselves (below), where our text is searched in its place. Each result links to the page image at the Library of Congress.",
      ),
    ).toBeTruthy();
  });

  it("says what part of the pages has text to search", async () => {
    const doc = {
      ...body,
      published: {
        ...body.published,
        indexes: ["pages-base-fixture"],
        deltas: 0,
        ja: { indexes: ["pages-ja-fixture"], fold: 2, pages: 50 },
      },
      indexing: indexing({
        runs: [indexRun({ index_version: "fixture-v1", docs: 1800, pages: 1872 })],
      }),
    };
    serve(doc);
    renderPage();
    expect(
      await screen.findByText(
        /^The site covers 1,872 newspaper pages .*\. The main index has text for 1,800 of these pages, and the Japanese index for 50\. A page with no text at all can't come up in a search\.$/,
      ),
    ).toBeTruthy();
    expect(
      screen.getByText(/^1,872 pages from 6 newspapers, dated .*, 1,800 of them with text in the main index$/),
    ).toBeTruthy();
    // When its build finished, not when it was published or the site switched to it.
    expect(screen.getByText(/^This is the version of the search index the site searches now \(/).textContent).toMatch(
      /\(fixture-v1\), whose build finished [A-Z][a-z]{2} \d{1,2} at \d{1,2}:\d\d [AP]M \S+\.$/,
    );
    // The Japanese index is in the index set.
    expect(screen.getByText("Index set:").parentElement!.textContent).toBe(
      "Index set: base pages-base-fixture; Japanese pages-ja-fixture.",
    );
  });

  it("counts languages by page language when the version has it, and by catalog language too", async () => {
    const doc = {
      ...body,
      published: {
        ...body.published,
        by_language: {
          pages_known: true,
          multilingual_titles: 0,
          multilingual_pages: 0,
          rows: [
            { code: "eng", name: "English", titles: 5, pages: 1500, percent: 80.1 },
            { code: "haw", name: "Hawaiian", titles: 1, pages: 372, percent: 19.9 },
          ],
        },
      },
    };
    const meta = {
      ...fixtureMeta,
      language_filter: "page",
      languages: [
        { code: "eng", name: "English", titles: 5, pages: 1860 },
        { code: "haw", name: "Hawaiian", titles: 1, pages: 12 },
      ],
    };
    serve(doc, noVersions, meta);
    renderPage();
    const byPage = await screen.findByRole("table", { name: "Published pages by language" });
    expect(
      within(byPage)
        .getAllByRole("row")
        .map((r) => r.textContent),
    ).toEqual(["LanguagePages ↓Share of pages", "English1,86099.4%", "Hawaiian120.6%"]);
    const byCatalog = screen.getByRole("table", { name: "Published pages by catalog language" });
    expect(within(byCatalog).getByText("372")).toBeTruthy();
    expect(screen.getByRole("heading", { level: 3, name: "Pages by catalog language" })).toBeTruthy();
    const section = screen.getByRole("region", { name: "Languages" });
    expect(section.textContent).toContain("which is what the search's language filter matches");
    // A catalog language with no word list is added to the detected language, not to every catalog language.
    expect(section.textContent).toContain(
      "A page too short or too garbled to tell counts in the languages the Library of Congress catalogs for its newspaper. A page of a newspaper cataloged in a language we have no word list for, such as Japanese, also counts in that language.",
    );
    expect(section.textContent).not.toMatch(/next full rebuild|#283/);
    cleanup();

    // A version without page languages: catalog languages alone, and no promise of a rebuild.
    serve(doc);
    renderPage();
    await screen.findByRole("table", { name: "Published pages by language" });
    expect(screen.queryByRole("heading", { level: 3, name: "Pages by catalog language" })).toBeNull();
    const titles = screen.getByRole("region", { name: "Languages" });
    expect(titles.textContent).toContain("Pages are counted by the languages the Library of Congress catalogs");
    expect(titles.textContent).not.toMatch(/next full rebuild|#283/);
  });

  it("marks the pipeline's version when it isn't the served one, and runs the ingest job didn't report", async () => {
    const doc = {
      ...body,
      activity: {
        available: true,
        now: "idle",
        source: "none",
        since: null,
        run_started_at: null,
        run: null,
        reported_at: null,
        done: null,
        total: null,
        percent: null,
        eta: null,
        paused_until: null,
        index_version: null,
        merge: null,
        last: {
          outcome: "failed",
          ended_at: "2026-09-25T15:01:01Z",
          step: "merging",
          error: "index holds too many documents",
          index_version: "fixture-v0",
        },
        next_run: null,
      },
      indexing: indexing({
        current_version: "fixture-v0",
        runs: [
          indexRun({ index_version: "fixture-v1", published_at: "2026-09-27T00:00:00Z" }),
          indexRun({ index_version: "fixture-v0", status: "failed", published_at: null, duration_secs: 600 }),
        ],
      }),
    };
    serve(doc);
    renderPage();
    const current = (await screen.findByText("Pipeline's current version")).parentElement!;
    expect(current.textContent).toBe(
      "Pipeline's current versionfixture-v0 (not the version the site searches, fixture-v1)",
    );
    // activity.last can be the newest index run when the job has no report, so the label names no source.
    expect(screen.getByText("Last reported run:").parentElement!.textContent).toMatch(
      /^Last reported run: failed .* \(fixture-v0\): index holds too many documents$/,
    );
    // A published run's time is when its build finished, which a staged run reaches before the site switches.
    expect(screen.getByText("A later index run:").parentElement!.textContent).toMatch(
      /^A later index run: build finished .* \(fixture-v1\), which the ingest job didn't report\./,
    );
    // The served version's own time, under Index builds, is labelled the same way.
    expect(screen.getByText("Build finished", { selector: "dt" })).toBeTruthy();
    expect(screen.queryByText("Published", { selector: "dt" })).toBeNull();
  });

  it("links Chronicling America at loc.gov and words the fixed text for the current index", async () => {
    serve(body);
    renderPage();
    await screen.findByRole("heading", { level: 2, name: "Places" });
    const links = screen.getAllByRole("link", { name: "Chronicling America" });
    expect(links.length).toBeGreaterThanOrEqual(2);
    for (const a of links) expect(a.getAttribute("href")).toBe("https://www.loc.gov/collections/chronicling-america/");
    const places = screen.getByRole("region", { name: "Places" });
    expect(places.textContent).toContain(
      "Known gaps: a few towns couldn't be placed from the sources we checked. A correction that moves a newspaper to another town takes effect at the next full rebuild; a corrected point or name for a town takes effect at the next update.",
    );
    expect(places.textContent).not.toContain("still shown at the town in their title");
    // The glossary's OCR entry: who runs it now, and a section that exists.
    const ocr = screen.getByText("OCR", { selector: "dt" }).parentElement!;
    expect(ocr.textContent).toContain("New OCR runs through BOINC");
    expect(ocr.textContent).not.toContain("OCR experiments");
  });

  it("gives the Japanese pages without usable text as a share of those newspapers' pages", async () => {
    serve({
      ...body,
      published: {
        ...body.published,
        by_language: {
          pages_known: true,
          multilingual_titles: 1,
          multilingual_pages: 32_829,
          rows: [{ code: "jpn", name: "Japanese", titles: 27, pages: 32_829, percent: 0.1 }],
        },
      },
      ocr_ja: {
        available: true,
        running: false,
        targets: { pages: 12_811, issues: 3690 },
        done: { pages: 12_768, issues: 3680 },
        percent: 99.6,
        engine: null,
        started_at: null,
        updated_at: new Date(Date.now() - 5 * 86_400_000).toISOString(),
        eta: null,
      },
    });
    renderPage();
    expect(
      await screen.findByText(
        "The Library of Congress has little or no searchable text for the pages printed in Japanese in its Japanese-language newspapers, mostly those of the World War II incarceration camps: 12,811 of their 32,829 pages. We read those page images ourselves with NDLOCR-Lite, text-recognition software from Japan's National Diet Library.",
      ),
    ).toBeTruthy();
  });

  it("shows a short error in full and a long one as a closed preview", () => {
    const short = "x".repeat(60);
    const long = `quickwit returned 503: ${"y".repeat(300)}`;
    const { container } = render(
      <>
        <ErrorText text={short} />
        <ErrorText text={long} />
        <ErrorText text={null} />
      </>,
    );
    // 60 characters: in full, with nothing to expand.
    expect(screen.getByText(short).tagName).toBe("SPAN");
    // Longer: a closed <details> whose summary is the first 60 characters.
    const details = container.querySelector("details.status-error")!;
    expect(details).toBeTruthy();
    expect(details.hasAttribute("open")).toBe(false);
    expect(details.querySelector("summary")!.textContent).toBe(`${long.slice(0, 60)}…`);
    expect(details.textContent).toContain(long);
    expect(container.querySelectorAll("details")).toHaveLength(1);
    // No error: a dash.
    expect(container.textContent!.endsWith("–")).toBe(true);
  });
});
