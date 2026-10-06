import { afterEach, describe, expect, it } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import type { ByLanguage, StatePages } from "../api/types";
import { LanguagesSection, StatesSection } from "./PagesTables";

afterEach(cleanup);

const STATES: StatePages[] = [
  {
    state: "IL",
    name: "Illinois",
    places: 2,
    titles: 3,
    pages: 12000,
    percent: 80,
  },
  {
    state: "NY",
    name: "New York",
    places: 1,
    titles: 1,
    pages: 2990,
    percent: 19.9,
  },
  {
    state: "VI",
    name: "Virgin Islands",
    places: 1,
    titles: 1,
    pages: 10,
    percent: 0.1,
  },
  {
    state: "DC",
    name: "District of Columbia",
    places: 1,
    titles: 1,
    pages: 3,
    percent: 0,
  },
];

/** Each body row's cells, as text. */
function bodyRows(table: HTMLElement): string[][] {
  const body = table.querySelector("tbody")!;
  return [...body.querySelectorAll("tr")].map((tr) =>
    [...tr.querySelectorAll("th, td")].map((c) => c.textContent ?? ""),
  );
}

describe("StatesSection", () => {
  it("keeps the table collapsed under the heading and summary until opened", () => {
    render(<StatesSection rows={STATES} pages={15003} />);
    const table = screen.getByRole("table", {
      name: "Published pages by state",
    });
    const details = table.closest("details")!;
    expect(details.open).toBe(false);
    expect(
      screen
        .getByText(/4 states and territories have pages/)
        .closest("details"),
    ).toBeNull();
    fireEvent.click(screen.getByText("Show the table of pages by state"));
    expect(details.open).toBe(true);
  });

  it("lists states by pages with separators, shares and a total", () => {
    render(<StatesSection rows={STATES} pages={15003} />);
    expect(
      screen.getByRole("heading", { level: 3, name: "Pages by state" }),
    ).toBeTruthy();
    expect(
      screen.getByText(/4 states and territories have pages/),
    ).toBeTruthy();
    const table = screen.getByRole("table", {
      name: "Published pages by state",
    });
    expect(bodyRows(table)).toEqual([
      ["Illinois", "2", "3", "12,000", "80.0%"],
      ["New York", "1", "1", "2,990", "19.9%"],
      ["Virgin Islands", "1", "1", "10", "0.1%"],
      ["District of Columbia", "1", "1", "3", "under 0.1%"],
    ]);
    // Row headers name each row; column headers carry the sort.
    expect(within(table).getAllByRole("rowheader")[0]!.textContent).toBe(
      "Illinois",
    );
    const pages = within(table).getByRole("columnheader", { name: "Pages" });
    expect(pages.getAttribute("aria-sort")).toBe("descending");
    expect(pages.getAttribute("scope")).toBe("col");
    const foot = table.querySelector("tfoot tr")!;
    expect([...foot.children].map((c) => c.textContent)).toEqual([
      "Total",
      "5",
      "6",
      "15,003",
      "100.0%",
    ]);
  });

  it("sorts by any column, alphabetically or by number", () => {
    render(<StatesSection rows={STATES} pages={15003} />);
    const table = screen.getByRole("table");
    fireEvent.click(within(table).getByRole("button", { name: "State" }));
    expect(bodyRows(table).map((r) => r[0])).toEqual([
      "District of Columbia",
      "Illinois",
      "New York",
      "Virgin Islands",
    ]);
    expect(
      within(table)
        .getByRole("columnheader", { name: "State" })
        .getAttribute("aria-sort"),
    ).toBe("ascending");
    fireEvent.click(within(table).getByRole("button", { name: "State" }));
    expect(bodyRows(table)[0]![0]).toBe("Virgin Islands");
    // Numbers sort largest first on the first click; ties go alphabetically.
    fireEvent.click(within(table).getByRole("button", { name: "Places" }));
    expect(bodyRows(table).map((r) => r[0])).toEqual([
      "Illinois",
      "District of Columbia",
      "New York",
      "Virgin Islands",
    ]);
  });

  it("says so when nothing is published", () => {
    render(<StatesSection rows={[]} pages={0} />);
    expect(screen.getByText("No pages are published yet.")).toBeTruthy();
    expect(screen.queryByRole("table")).toBeNull();
  });
});

const LANGUAGES: ByLanguage = {
  pages_known: true,
  multilingual_titles: 1,
  multilingual_pages: 40,
  rows: [
    { code: "eng", name: "English", titles: 3, pages: 141, percent: 88.1 },
    { code: "ger", name: "German", titles: 2, pages: 50, percent: 31.3 },
    { code: null, name: "Not recorded", titles: 1, pages: 5, percent: 3.1 },
  ],
};

describe("LanguagesSection", () => {
  it("keeps the table collapsed until opened", () => {
    render(<LanguagesSection data={LANGUAGES} />);
    const details = screen
      .getByRole("table", { name: "Published pages by language" })
      .closest("details")!;
    expect(details.open).toBe(false);
    fireEvent.click(screen.getByText("Show the table of pages by language"));
    expect(details.open).toBe(true);
  });

  it("counts a bilingual newspaper in each language and says so", () => {
    render(<LanguagesSection data={LANGUAGES} />);
    expect(
      screen.getByRole("heading", { level: 3, name: "Pages by language" }),
    ).toBeTruthy();
    expect(
      screen.getByText(/1 newspaper lists more than one language \(40 pages\)/)
        .textContent,
    ).toContain("the rows add up to more than all published pages");
    const table = screen.getByRole("table", {
      name: "Published pages by language",
    });
    expect(bodyRows(table)).toEqual([
      ["English", "3", "141", "88.1%"],
      ["German", "2", "50", "31.3%"],
      ["Not recorded", "1", "5", "3.1%"],
    ]);
    expect(table.querySelector("tfoot")).toBeNull();
  });

  it("shows newspaper counts only for a version without pages per newspaper", () => {
    const old: ByLanguage = {
      pages_known: false,
      multilingual_titles: 0,
      multilingual_pages: null,
      rows: LANGUAGES.rows.map((r) => ({ ...r, pages: null, percent: null })),
    };
    render(<LanguagesSection data={old} />);
    expect(screen.getByRole("note").textContent).toContain(
      "Page counts appear once a newer search index goes live",
    );
    const table = screen.getByRole("table", {
      name: "Published newspapers by language",
    });
    expect(
      within(table)
        .getAllByRole("columnheader")
        .map((h) => h.textContent?.replace(/ [↑↓]$/, "")),
    ).toEqual(["Language", "Newspapers"]);
    expect(
      within(table)
        .getByRole("columnheader", { name: "Newspapers" })
        .getAttribute("aria-sort"),
    ).toBe("descending");
    expect(bodyRows(table)[0]).toEqual(["English", "3"]);
  });

  it("goes back to its initial order when the sorted column goes away", () => {
    const { rerender } = render(<LanguagesSection data={LANGUAGES} />);
    const share = () => screen.getByRole("button", { name: /Share of pages/ });
    fireEvent.click(share());
    expect(share().textContent).toContain("↓");
    // A rollback to a version without page counts, while the page is open.
    const old: ByLanguage = {
      ...LANGUAGES,
      pages_known: false,
      multilingual_pages: null,
      rows: LANGUAGES.rows.map((r) => ({ ...r, pages: null, percent: null })),
    };
    rerender(<LanguagesSection data={old} />);
    const table = screen.getByRole("table");
    expect(
      within(table)
        .getByRole("columnheader", { name: /Newspapers/ })
        .getAttribute("aria-sort"),
    ).toBe("descending");
    expect(bodyRows(table).map((r) => r[0])).toEqual([
      "English",
      "German",
      "Not recorded",
    ]);
  });
});
