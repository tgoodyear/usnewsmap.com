import { expect, test } from "@playwright/test";

test("States first-paint proxy meets 300 ms after the boundary asset arrives", async ({
  playwright,
  isMobile,
  browserName,
}, testInfo) => {
  test.skip(process.env.USNM_STATES_PERF !== "1" || isMobile, "Opt-in desktop hardware measurement");
  const samples: number[] = [];
  const drawSamples: number[] = [];
  const parseSamples: number[] = [];
  const shaderVariants: number[] = [];
  const shaderCold = process.env.USNM_STATES_SHADER_COLD === "1";
  let userAgent = "";
  let renderer = "";
  for (let i = 0; i < 5; i++) {
    // New processes isolate browser caches, not the OS/driver's persistent shader cache.
    const browser = await playwright[browserName].launch({
      executablePath: process.env.PW_CHROMIUM_PATH,
      args: [
        ...(process.env.USNM_STATES_GPU === "metal" ? ["--use-angle=metal"] : []),
        "--disable-gpu-shader-disk-cache",
        "--disable-gpu-program-cache",
      ],
    });
    try {
      const page = await browser.newPage({
        baseURL: testInfo.project.use.baseURL,
        viewport: testInfo.project.use.viewport,
      });
      if (shaderCold) {
        await page.addInitScript(
          (offset) => {
            const original = WebGL2RenderingContext.prototype.shaderSource;
            WebGL2RenderingContext.prototype.shaderSource = function (shader, source) {
              if (/SHADER_NAME (solid-polygon|path|line)-layer-vertex-shader/.test(source)) {
                source = source.replace(/void main\s*\(\s*(?:void)?\s*\)\s*\{/, "void measuredOriginalMain() {");
                // A unique, subpixel position offset changes the executable shader, not just a comment/cache key.
                source += `\nvoid main() { measuredOriginalMain(); gl_Position.x += ${offset} * gl_Position.w; }\n`;
                performance.mark("states-diagnostic-shader-variant");
              }
              return original.call(this, shader, source);
            };
          },
          (1e-8 + Math.random() * 1e-8).toPrecision(9),
        );
      }
      await page.route("https://tiles.openfreemap.org/**", (route) =>
        route.fulfill({
          json: {
            version: 8,
            sources: {},
            layers: [{ id: "bg", type: "background", paint: { "background-color": "#e8e4dc" } }],
          },
        }),
      );
      await page.goto("/?q=%22cross+of+gold%22&bucket=month");
      await expect(page.getByTestId("map")).toBeVisible();
      // Warm the map and let the already-requested coverage arrive, not the state geometry.
      await page.waitForTimeout(500);
      await expect(page.locator("#search-overlay")).toHaveCSS("opacity", "1");
      expect(
        await page.evaluate(() => performance.getEntriesByType("resource").some((e) => e.name.endsWith(".geojson"))),
      ).toBe(false);
      await page.getByRole("combobox", { name: "Map layer" }).selectOption("states");
      await page.waitForFunction(() => performance.getEntriesByName("usnm-states-first-paint").length > 0);
      samples.push(await page.evaluate(() => performance.getEntriesByName("usnm-states-first-paint")[0]!.duration));
      drawSamples.push(await page.evaluate(() => performance.getEntriesByName("usnm-states-first-draw")[0]!.duration));
      parseSamples.push(
        await page.evaluate(() => {
          const resource = performance
            .getEntriesByType("resource")
            .find((e) => e.name.endsWith(".geojson")) as PerformanceResourceTiming;
          return performance.getEntriesByName("usnm-states-parsed")[0]!.startTime - resource.responseEnd;
        }),
      );
      shaderVariants.push(
        await page.evaluate(() => performance.getEntriesByName("states-diagnostic-shader-variant").length),
      );
      userAgent = await page.evaluate(() => navigator.userAgent);
      renderer = await page.evaluate(() => {
        const gl = document.createElement("canvas").getContext("webgl2");
        const info = gl?.getExtension("WEBGL_debug_renderer_info");
        return info ? (gl!.getParameter(info.UNMASKED_RENDERER_WEBGL) as string) : "unknown";
      });
    } finally {
      await browser.close();
    }
  }
  console.log(
    JSON.stringify({
      kind: "asset responseEnd to first deck draw plus two animation frames",
      hardware: "runner-specific; record host separately",
      browserName,
      cacheScope: "fresh browser processes; Chrome GPU caches disabled; OS/driver cache uncontrolled",
      shaderCold,
      shaderVariants,
      userAgent,
      renderer,
      samplesMs: samples,
      drawSamplesMs: drawSamples,
      parseSamplesMs: parseSamples,
      maxMs: Math.max(...samples),
    }),
  );
  if (shaderCold) expect(shaderVariants).toEqual([2, 2, 2, 2, 2]);
  expect(Math.max(...samples)).toBeLessThanOrEqual(300);
});
