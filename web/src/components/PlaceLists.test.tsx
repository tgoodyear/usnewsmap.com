import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import {
  PagesLists,
  WhenLists,
  exactMedian,
  medianCandidates,
  medianExtremes,
  mostPages,
  statePages,
  type ListRow,
} from "./PlaceLists";
import { dayNumber } from "../lib/time";

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

describe("ties and windows", () => {
  it("never lists a place in both lists, even when every median is the same", () => {
    const same = ["A", "B", "C", "D", "E", "F"].map((id, i) => row(id, "IL", 10 + i, 0.5, "Jun 1896"));
    const { earliest, latest } = medianExtremes(same);
    expect(earliest).toHaveLength(3);
    expect(latest).toHaveLength(3);
    expect(earliest.filter((r) => latest.includes(r))).toEqual([]);
  });

  it("says when the counts are a trailing window", () => {
    render(<PagesLists rows={[]} onSelect={() => {}} trailing={true} />);
    expect(screen.getAllByText("No matching pages in the playback window.")).toHaveLength(2);
    cleanup();
    render(<WhenLists rows={ROWS} onSelect={() => {}} trailing={true} />);
    expect(screen.getByText(/half of its matching pages in the playback window had been printed/)).toBeTruthy();
  });
});

describe("PagesLists", () => {
  it("names each place's pages and share, and selects a place", () => {
    const onSelect = vi.fn();
    render(<PagesLists rows={ROWS} onSelect={onSelect} trailing={false} />);
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
    render(<WhenLists rows={ROWS} onSelect={() => {}} trailing={false} />);
    expect(screen.getByText("Jan 1875 · 300 pages")).toBeTruthy();
    cleanup();
    render(<WhenLists rows={[row("Reno", "NV", 4, 0, "Jan 1870")]} onSelect={() => {}} trailing={false} />);
    expect(screen.getAllByText("No place has 5 or more matching pages up to the current date.")).toHaveLength(2);
  });
});

describe("hiding the lists", () => {
  it("hides and shows the panel, and remembers it for the next visit", () => {
    render(<PagesLists rows={ROWS} onSelect={() => {}} trailing={false} />);
    const hide = screen.getByRole("button", { name: "Hide" });
    expect(hide.getAttribute("aria-expanded")).toBe("true");
    fireEvent.click(hide);
    expect(screen.queryByRole("heading", { name: "Most pages" })).toBeNull();
    const show = screen.getByRole("button", { name: "Show most pages" });
    expect(show.getAttribute("aria-expanded")).toBe("false");
    expect(window.localStorage.getItem("usnm.lists")).toBe("hidden");

    // Hidden in one measure, hidden in the others too, and after a reload.
    cleanup();
    render(<WhenLists rows={ROWS} onSelect={() => {}} trailing={false} />);
    expect(screen.getByRole("button", { name: "Show earliest and latest" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Show earliest and latest" }));
    expect(screen.getByRole("heading", { name: "Earliest and latest" })).toBeTruthy();
    expect(window.localStorage.getItem("usnm.lists")).toBeNull();
  });
});

describe("exact median days", () => {
  it("asks for the places that could make either list, ties at the fifth included", () => {
    // Six eligible places: all of them could be in one list or the other.
    expect(medianCandidates(ROWS)).toEqual(["Albany", "Boise", "Buffalo", "Chicago", "Peoria", "Tucson"]);
    // Twelve places in one month: the ten with the most pages.
    const same = Array.from({ length: 12 }, (_, i) => row(`P${String(i).padStart(2, "0")}`, "IL", 10 + i, 0.5, "Jun 1896"));
    const c = medianCandidates(same);
    expect(c).toHaveLength(10);
    expect(c).not.toContain("P00");
    expect(c).not.toContain("P01");
  });

  it("finds the first day by which half the window's pages had been printed", () => {
    const days = [100, 105, 110, 120];
    const hits = [1, 1, 2, 4];
    expect(exactMedian(days, hits, -Infinity, Infinity)).toBe(110); // 8 pages: the 4th is on day 110
    expect(exactMedian(days, hits, 105, 115)).toBe(110); // 3 pages in the window: half (1.5) by day 110
    expect(exactMedian(days, hits, 106, 109)).toBeNaN();
  });

  it("ranks and labels by the exact day once it is known", () => {
    const rows = [row("Early", "IL", 10, 0.1, "Jan 1895"), row("Later", "IL", 10, 0.1, "Jan 1895")];
    const exact = new Map([
      ["Early", dayNumber("1895-01-03")],
      ["Later", dayNumber("1895-01-28")],
    ]);
    render(<WhenLists rows={rows} onSelect={() => {}} trailing={false} exact={exact} />);
    const earliest = screen.getByRole("heading", { name: "Earliest median date" }).parentElement!;
    expect(earliest.textContent).toContain("Early, IL Jan 3, 1895 · 10 pages");
    const latest = screen.getByRole("heading", { name: "Latest median date" }).parentElement!;
    expect(latest.textContent).toContain("Later, IL Jan 28, 1895 · 10 pages");
  });
});
