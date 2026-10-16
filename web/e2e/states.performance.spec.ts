import { expect, test } from "@playwright/test";

test.use({
  launchOptions: process.env.USNM_STATES_GPU === "metal" ? { args: ["--use-angle=metal"] } : undefined,
});

test("States first-paint proxy meets 300 ms after the boundary asset arrives", async ({
  page,
  isMobile,
  browserName,
}) => {
  test.skip(process.env.USNM_STATES_PERF !== "1" || isMobile, "Opt-in desktop hardware measurement");
  await page.route("https://tiles.openfreemap.org/**", (route) =>
    route.fulfill({
      json: {
        version: 8,
        sources: {},
        layers: [{ id: "bg", type: "background", paint: { "background-color": "#e8e4dc" } }],
      },
    }),
  );
  const samples: number[] = [];
  for (let i = 0; i < 5; i++) {
    await page.goto("/?q=%22cross+of+gold%22&bucket=month");
    await expect(page.getByTestId("map")).toBeVisible();
    // Warm the map and let the already-requested coverage arrive, not the state geometry.
    await page.waitForTimeout(500);
    await page.getByRole("combobox", { name: "Map layer" }).selectOption("states");
    await page.waitForFunction(() => performance.getEntriesByName("usnm-states-first-paint").length > 0);
    samples.push(await page.evaluate(() => performance.getEntriesByName("usnm-states-first-paint")[0]!.duration));
  }
  console.log(
    JSON.stringify({
      kind: "asset responseEnd to first deck draw plus two animation frames",
      hardware: "runner-specific; record host separately",
      browserName,
      userAgent: await page.evaluate(() => navigator.userAgent),
      renderer: await page.evaluate(() => {
        const gl = document.createElement("canvas").getContext("webgl2");
        const info = gl?.getExtension("WEBGL_debug_renderer_info");
        return info ? (gl!.getParameter(info.UNMASKED_RENDERER_WEBGL) as string) : "unknown";
      }),
      samplesMs: samples,
      maxMs: Math.max(...samples),
    }),
  );
  expect(Math.max(...samples)).toBeLessThanOrEqual(300);
});
