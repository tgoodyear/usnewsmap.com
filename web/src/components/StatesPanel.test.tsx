import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { StatesLegend, StatesTable, statesCsv } from "./StatesPanel";
const rows = [
  { code: "NY", name: "New York", hits: 10, pages: 200, rate: 50 },
  { code: "DC", name: "District of Columbia", hits: 0, pages: 50, rate: 0 },
  { code: "", name: "Unassigned", hits: 1, pages: 0, rate: null },
];
afterEach(cleanup);
it("labels all five classes, zero matches, no pages and modern borders", () => {
  render(<StatesLegend breaks={[1, 2, 3, 4, 5]} />);
  expect(screen.getAllByRole("listitem")).toHaveLength(7);
  expect(screen.getByText("No matches")).toBeDefined();
  expect(screen.getByText("No pages in this window")).toBeDefined();
  expect(screen.getByText(/Modern state borders/)).toBeDefined();
  expect(screen.getByText(/Matching pages per 1,000 pages published/)).toBeDefined();
});
it("sorts, keeps all states accessible, adds a filter and exports the same raw numbers", () => {
  const filter = vi.fn();
  render(<StatesTable rows={rows} onFilter={filter} />);
  fireEvent.click(screen.getByRole("button", { name: "Matching pages" }));
  expect(within(screen.getAllByRole("row")[1]!).getByText("New York")).toBeDefined();
  fireEvent.click(screen.getByRole("button", { name: "Add state filter: New York" }));
  expect(filter).toHaveBeenCalledWith("NY");
  expect(screen.getByText("Unassigned")).toBeDefined();
  expect(statesCsv(rows)).toContain('"NY","New York","10","200","50"\r\n');
  expect(statesCsv(rows)).toContain('"","Unassigned","1","0",""\r\n');
  expect(screen.getByRole("button", { name: "Download states CSV" })).toBeDefined();
});
