import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { BoincSection, parseSummary } from "./BoincSection";

const now = Date.parse("2026-10-18T15:00:00Z");
const summary = {
  schema: 1,
  updated_at: "2026-10-18T14:59:00Z",
  apps: [
    { name: "usnm_qwen", ready: 469, in_progress: 2, validated: 17 },
    { name: "usnm_gold", ready: 0, in_progress: 9, validated: 3 },
    { name: "usnm_ocr", ready: 0, in_progress: 0, validated: 0 },
  ],
};

function show(value: unknown = summary, status = 200) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(JSON.stringify(value), { status })),
  );
  render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <BoincSection now={now} />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("BOINC summary", () => {
  it("labels totals and current applications in tasks, without implying publication", async () => {
    show();
    const table = await screen.findByRole("table", { name: "BOINC tasks by application" });
    expect(within(table).getByText("Qwen transcription experiments")).toBeTruthy();
    expect(within(table).getAllByRole("row")).toHaveLength(3);
    expect(screen.getByText("469 ready to send")).toBeTruthy();
    expect(screen.getByText("11 in progress")).toBeTruthy();
    expect(screen.getByText("20 completed and validated")).toBeTruthy();
    expect(screen.getByText(/not lifetime totals or the full unqueued backlog/)).toBeTruthy();
    expect(screen.getByText(/not that its text is accepted or searchable/)).toBeTruthy();
    expect(fetch).toHaveBeenCalledWith(
      "https://boinc.usnewsmap.com/work_summary.php",
      expect.objectContaining({
        credentials: "omit",
      }),
    );
  });

  it("distinguishes a valid zero report from unavailable counts", async () => {
    show({ ...summary, apps: [] });
    expect(await screen.findByText("0 ready to send")).toBeTruthy();
    expect(
      screen.getByText("No ready, in-progress or completed-and-validated tasks are currently recorded."),
    ).toBeTruthy();
  });

  it("does not call the database empty when applications have only undisplayed task states", async () => {
    show({ ...summary, apps: [{ name: "usnm_qwen", ready: 0, in_progress: 0, validated: 0 }] });
    expect(
      await screen.findByText("No ready, in-progress or completed-and-validated tasks are currently recorded."),
    ).toBeTruthy();
    expect(screen.queryByText(/database.*empty|no tasks are recorded/i)).toBeNull();
  });

  it("marks old readings as out of date", async () => {
    show({ ...summary, updated_at: "2026-10-17T14:59:00Z" });
    expect(await screen.findByText("BOINC counts are out of date; showing the last report.")).toBeTruthy();
  });

  it.each([
    null,
    {},
    { ...summary, schema: 2 },
    { ...summary, updated_at: "invalid" },
    { ...summary, apps: [{ ...summary.apps[0], ready: -1 }] },
    { ...summary, apps: [{ ...summary.apps[0], validated: "2" }] },
    { ...summary, apps: [summary.apps[0], summary.apps[0]] },
  ])("rejects invalid reports rather than showing zeros: %j", (value) => {
    expect(() => parseSummary(value)).toThrow();
  });

  it("shows request failures explicitly", async () => {
    show({}, 503);
    expect(
      await screen.findByText("BOINC task counts are temporarily unavailable.", {}, { timeout: 4000 }),
    ).toBeTruthy();
    expect(screen.queryByText("0 ready to send")).toBeNull();
  });
});
