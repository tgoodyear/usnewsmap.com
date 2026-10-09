import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import type { Meta } from "../api/types";
import { sameSearch, type SearchParams } from "../api/client";
import { DEFAULTS } from "../state/url";
import { SearchBar } from "./SearchBar";
import { quoted, Searching } from "./Searching";

afterEach(cleanup);

const meta = {
  bounds: { from: "1736-01-01", to: "1963-12-31" },
  limits: { max_query_chars: 256 },
} as unknown as Meta;

describe("a running search (#210)", () => {
  it("is a new search when the words or filters change, not the bucket", () => {
    const a: SearchParams = {
      q: "cross of gold",
      mode: "phrase",
      from: "1890-01-01",
      bucket: "month",
      state: ["NE"],
    };
    expect(sameSearch(a, { ...a, bucket: "year" })).toBe(true);
    expect(sameSearch(a, { ...a, q: "free silver" })).toBe(false);
    expect(sameSearch(a, { ...a, state: ["KS"] })).toBe(false);
    expect(sameSearch(a, { ...a, from: "1896-01-01" })).toBe(false);
    expect(sameSearch(a, { ...a, lang: ["ger"] })).toBe(false);
  });

  it("says what it is searching for, with the queue note when there is one", () => {
    render(<Searching q="cross of gold" note={null} />);
    const status = screen.getByRole("status");
    expect(status.textContent).toBe("Searching for “cross of gold”…");
    cleanup();
    render(
      <Searching
        q="lincoln"
        note="Other searches are running. Yours is next in line…"
      />,
    );
    expect(screen.getByRole("status").textContent).toContain(
      "Yours is next in line",
    );
  });

  it("shows a query's own quotes as curly ones instead of adding a second pair", () => {
    expect(quoted("cross of gold")).toBe("“cross of gold”");
    expect(quoted('"lend lease"')).toBe("“lend lease”");
    expect(quoted('"new york" fire')).toBe("“new york” fire");
    expect(quoted('fire "new york" "brooklyn')).toBe(
      "fire “new york” “brooklyn",
    );
    render(<Searching q={'"lend lease"'} note={null} />);
    expect(screen.getByRole("status").textContent).toBe(
      "Searching for “lend lease”…",
    );
  });

  it("puts a spinner in the Search button without changing its name", () => {
    const { rerender } = render(
      <SearchBar
        view={{ ...DEFAULTS, q: "gold" }}
        meta={meta}
        onSearch={() => {}}
      />,
    );
    const button = screen.getByRole("button", { name: "Search" });
    expect(button.getAttribute("aria-busy")).toBe("false");
    expect(button.classList.contains("search__go--busy")).toBe(false);
    rerender(
      <SearchBar
        view={{ ...DEFAULTS, q: "gold" }}
        meta={meta}
        onSearch={() => {}}
        busy
      />,
    );
    expect(
      screen.getByRole("button", { name: "Search" }).getAttribute("aria-busy"),
    ).toBe("true");
    expect(button.classList.contains("search__go--busy")).toBe(true);
  });
});
