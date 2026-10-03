import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MeasureToggle } from "./MeasureToggle";
import { SkewLegend } from "./SkewLegend";
import { clearest, skewCsv, SkewLists, StateTable, type SkewRow } from "./SkewPanels";
import { PlaceTable } from "./PlaceTable";
import type { SkewInfo } from "../lib/skewText";

const info = (estimate: number, lower: number, upper: number, extra: Partial<SkewInfo> = {}): SkewInfo => ({
  estimate,
  lower,
  upper,
  observed: 10,
  expected: 5,
  pages: 100,
  dir: lower > 1 ? 1 : upper < 1 ? -1 : 0,
  languages: null,
  ...extra,
});
const row = (id: string, s: SkewInfo): SkewRow => ({ id, name: `Place ${id}`, state: "AL", skew: s });

describe("MeasureToggle", () => {
  afterEach(cleanup);

  it("offers pages and relative rate, and reports the choice", () => {
    const onChange = vi.fn();
    render(<MeasureToggle norm="raw" onChange={onChange} />);
    const group = screen.getByRole("group", { name: "Measure" });
    const buttons = Array.from(group.querySelectorAll("button"));
    expect(buttons.map((b) => [b.textContent, b.getAttribute("aria-pressed")])).toEqual([
      ["Pages", "true"],
      ["Relative rate", "false"],
    ]);
    fireEvent.click(screen.getByRole("button", { name: "Relative rate" }));
    expect(onChange).toHaveBeenCalledWith("skew");
  });

  it("has no share of pages button", () => {
    render(<MeasureToggle norm="skew" onChange={() => undefined} />);
    expect(screen.queryByRole("button", { name: "Share of pages" })).toBeNull();
    expect(screen.getAllByRole("button", { pressed: true }).map((b) => b.textContent)).toEqual(["Relative rate"]);
  });

  it("explains the measure in a disclosure that Escape closes", () => {
    render(<MeasureToggle norm="skew" onChange={() => undefined} />);
    const info = screen.getByRole("button", { name: "About the relative rate" });
    expect(info.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(info);
    expect(info.getAttribute("aria-expanded")).toBe("true");
    const body = document.getElementById(info.getAttribute("aria-controls")!)!;
    expect(body.hidden).toBe(false);
    expect(body.textContent).toContain("pulled toward the typical rate");
    fireEvent.keyDown(document, { key: "Escape" });
    expect(info.getAttribute("aria-expanded")).toBe("false");
  });
});

describe("SkewLegend", () => {
  afterEach(cleanup);

  it("says what was compared and over what, without the index version", () => {
    const { container } = render(<SkewLegend places={398} states={42} unit="month" />);
    const text = container.textContent ?? "";
    expect(text).toContain("compared with the other 397 places with pages in this window, in 42 states, over the same months");
    expect(text).toContain("1× is the same rate");
    expect(text).not.toContain("Index");
    expect(text).not.toMatch(/[—]/);
  });

  it("says when there is nothing to compare with", () => {
    const { container } = render(<SkewLegend places={1} states={1} unit="year" />);
    expect(container.textContent).toContain("No other place has pages in this window");
    cleanup();
    const two = render(<SkewLegend places={2} states={1} unit="year" />);
    expect(two.container.textContent).toContain("the other 1 place with pages in this window, in 1 state, over the same years");
  });
});

describe("lists, tables and export", () => {
  afterEach(cleanup);
  const rows = [
    row("a", info(3, 2.5, 3.5)),
    row("b", info(6, 1.2, 12)),
    row("c", info(0.3, 0.2, 0.45)),
    row("d", info(0.1, 0.05, 0.2, { languages: "Papers in German" })),
    row("e", info(1.1, 0.6, 1.8)),
  ];

  it("lists only clear differences, by the bound nearest 1×, naming papers in other languages", () => {
    const { above, below } = clearest(rows);
    expect(above.map((r) => r.id)).toEqual(["a", "b"]);
    expect(below.map((r) => r.id)).toEqual(["d", "c"]);
    const onSelect = vi.fn();
    render(<SkewLists rows={rows} onSelect={onSelect} />);
    fireEvent.click(screen.getByRole("button", { name: "Place a, AL" }));
    expect(onSelect).toHaveBeenCalledWith("a");
    expect(screen.getByRole("complementary", { name: "Places that differ most clearly" })).toBeTruthy();
    expect(screen.getByText("Papers in German")).toBeTruthy();
  });

  it("tables places and states with expected counts and ranges", () => {
    const tableRows = rows.map((r) => ({ ...r, precision: "city", position: [0, 0] as [number, number], value: 10, rel: 0.1, firstDay: 0 }));
    render(<PlaceTable skew rows={tableRows} onSelect={() => undefined} selected="" />);
    const headers = screen.getAllByRole("columnheader").map((h) => h.textContent);
    expect(headers).toEqual(["Place", "State", "Pages published", "Matched", "Expected", "Relative rate", "90% range", "Languages"]);
    // Sorted by relative rate, highest first.
    expect(screen.getAllByRole("rowheader").map((h) => h.textContent)).toEqual(["Place b", "Place a", "Place e", "Place c", "Place d"]);
    expect(screen.getByText("0.60 to 1.8× (can't tell)")).toBeTruthy();
    cleanup();
    render(<StateTable rows={[{ state: "AL", skew: info(2, 1.5, 2.5) }]} />);
    expect(screen.getByRole("table", { name: "By state, each compared with the other states" })).toBeTruthy();
  });

  it("exports one CSV row per place", () => {
    const csv = skewCsv([row("P1", info(6.14, 5.23, 7.15)), { ...row("P2", info(1, 0.5, 2, { languages: "Papers in German, Serbian and English" })), name: 'A, "B"' }]);
    expect(csv.split("\n")).toEqual([
      "place_id,name,state,pages,hits,expected,estimate,lower,upper,languages",
      "P1,Place P1,AL,100,10,5,6.14,5.23,7.15,",
      'P2,"A, ""B""",AL,100,10,5,1,0.5,2,"Papers in German, Serbian and English"',
      "",
    ]);
  });
});
