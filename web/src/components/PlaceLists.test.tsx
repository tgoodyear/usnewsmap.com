import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { PagesLists, WhenLists, medianExtremes, mostPages, statePages, type ListRow } from "./PlaceLists";

beforeEach(() => window.localStorage.clear());
afterEach(cleanup);

const row = (id: string, state: string, value: number, when = NaN, whenLabel = ""): ListRow => ({
  id,
  name: id,
  state,
  value,
  when,
  whenLabel,
});

const ROWS: ListRow[] = [
  row("Chicago", "IL", 500, 0.6, "Jun 1896"),
  row("Peoria", "IL", 40, 0.2, "Feb 1880"),
  row("Albany", "NY", 300, 0.9, "Oct 1910"),
  row("Buffalo", "NY", 300, 0.1, "Jan 1875"),
  row("Reno", "NV", 4, 0.0, "Jan 1870"),
  row("Tucson", "AZ", 20, 0.5, "Jan 1895"),
  row("Boise", "ID", 10, 0.4, "Jan 1890"),
  row("Empty", "OR", 0),
];

describe("the lists", () => {
  it("ranks places and states by pages, ties by name, and skips places with none", () => {
    expect(mostPages(ROWS).map((r) => r.id)).toEqual(["Chicago", "Albany", "Buffalo", "Peoria", "Tucson"]);
    expect(statePages(ROWS).slice(0, 2)).toEqual([
      { state: "NY", pages: 600, places: 2 },
      { state: "IL", pages: 540, places: 2 },
    ]);
    expect(statePages(ROWS).some((s) => s.state === "OR")).toBe(false);
  });

  it("lists earliest and latest median dates among places with enough pages, never one in both", () => {
    const { earliest, latest, eligible } = medianExtremes(ROWS);
    // Reno has 4 pages, under the minimum of 5; Empty has none.
    expect(eligible).toBe(6);
    expect(earliest.map((r) => r.id)).toEqual(["Buffalo", "Peoria", "Boise"]);
    expect(latest.map((r) => r.id)).toEqual(["Albany", "Chicago", "Tucson"]);
  });
});

describe("PagesLists", () => {
  it("names each place's pages and share, and selects a place", () => {
    const onSelect = vi.fn();
    render(<PagesLists rows={ROWS} onSelect={onSelect} />);
    const panel = screen.getByRole("complementary", { name: "Places and states with the most pages" });
    expect(within(panel).getByRole("heading", { level: 2, name: "Most pages" })).toBeTruthy();
    expect(panel.textContent).toContain("500 pages (42.6%)");
    expect(panel.textContent).toContain("600 pages (51.1%) in 2 places");
    fireEvent.click(within(panel).getByRole("button", { name: "Chicago, IL" }));
    expect(onSelect).toHaveBeenCalledWith("Chicago");
  });
});

describe("WhenLists", () => {
  it("shows each place's median date and says when none qualify", () => {
    render(<WhenLists rows={ROWS} onSelect={() => {}} />);
    expect(screen.getByText("Jan 1875 · 300 pages")).toBeTruthy();
    cleanup();
    render(<WhenLists rows={[row("Reno", "NV", 4, 0, "Jan 1870")]} onSelect={() => {}} />);
    expect(screen.getAllByText("No place has 5 or more matching pages up to the current date.")).toHaveLength(2);
  });
});

describe("hiding the lists", () => {
  it("hides and shows the panel, and remembers it for the next visit", () => {
    render(<PagesLists rows={ROWS} onSelect={() => {}} />);
    const hide = screen.getByRole("button", { name: "Hide" });
    expect(hide.getAttribute("aria-expanded")).toBe("true");
    fireEvent.click(hide);
    expect(screen.queryByRole("heading", { name: "Most pages" })).toBeNull();
    const show = screen.getByRole("button", { name: "Show most pages" });
    expect(show.getAttribute("aria-expanded")).toBe("false");
    expect(window.localStorage.getItem("usnm.lists")).toBe("hidden");

    // Hidden in one measure, hidden in the others too, and after a reload.
    cleanup();
    render(<WhenLists rows={ROWS} onSelect={() => {}} />);
    expect(screen.getByRole("button", { name: "Show earliest and latest" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Show earliest and latest" }));
    expect(screen.getByRole("heading", { name: "Earliest and latest" })).toBeTruthy();
    expect(window.localStorage.getItem("usnm.lists")).toBeNull();
  });
});
