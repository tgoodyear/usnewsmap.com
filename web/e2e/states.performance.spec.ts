import { expect, test } from "@playwright/test";
import { cpus, platform, arch } from "node:os";

for (const direct of [false, true]) {
  test(`States pixels reach a composited frame within 300 ms after the boundary asset arrives (${direct ? "direct URL" : "layer selection"})`, async ({
    playwright,
    isMobile,
    browserName,
  }, testInfo) => {
    test.skip(
      process.env.USNM_STATES_PERF !== "1" || isMobile || browserName !== "chromium",
      "Opt-in desktop Chromium hardware measurement",
    );
    const samples: number[] = [];
    const visibleProofSamples: number[] = [];
    const visiblePixels: number[][] = [];
    const drawSamples: number[] = [];
    const parseSamples: number[] = [];
    const shaderVariants: number[] = [];
    const shaderStalls: {
      totalMs: number;
      maxMs: number;
      calls: { name: string; duration: number; stack?: string }[];
    }[] = [];
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
        const frames: { data: string; timestamp: number }[] = [];
        const cdp = await page.context().newCDPSession(page);
        cdp.on("Page.screencastFrame", ({ data, metadata, sessionId }) => {
          if (metadata.timestamp !== undefined) frames.push({ data, timestamp: metadata.timestamp * 1000 });
          void cdp.send("Page.screencastFrameAck", { sessionId });
        });
        await cdp.send("Page.startScreencast", { format: "png", everyNthFrame: 1 });
        if (shaderCold) {
          await page.addInitScript(
            (offset) => {
              const original = WebGL2RenderingContext.prototype.shaderSource;
              WebGL2RenderingContext.prototype.shaderSource = function (shader, source) {
                if (this.getShaderParameter(shader, this.SHADER_TYPE) === this.VERTEX_SHADER) {
                  source = source.replace(/void main\s*\(\s*(?:void)?\s*\)\s*\{/, "void measuredOriginalMain() {");
                  // A unique, subpixel position offset changes the executable shader, not just a comment/cache key.
                  source += `\nvoid main() { measuredOriginalMain(); gl_Position.x += ${offset} * gl_Position.w; }\n`;
                  performance.mark("states-diagnostic-shader-variant");
                }
                return original.call(this, shader, source);
              };
              for (const name of [
                "compileShader",
                "linkProgram",
                "getProgramParameter",
                "getShaderParameter",
              ] as const) {
                const prototype = WebGL2RenderingContext.prototype;
                const call = prototype[name] as (...args: unknown[]) => unknown;
                Object.defineProperty(prototype, name, {
                  value: function (this: WebGL2RenderingContext, ...args: unknown[]) {
                    const start = performance.now();
                    try {
                      return call.apply(this, args);
                    } finally {
                      const parameter =
                        name === "getProgramParameter" || name === "getShaderParameter"
                          ? `-0x${Number(args[1]).toString(16)}`
                          : "";
                      const end = performance.now();
                      performance.measure(`states-diagnostic-gl-${name}${parameter}`, {
                        start,
                        end,
                        detail: end - start >= 1 ? { stack: new Error().stack } : undefined,
                      });
                    }
                  },
                  configurable: true,
                  writable: true,
                });
              }
            },
            (1e-8 + Math.random() * 1e-8).toPrecision(9),
          );
        }
        await page.route(/tiles\.openfreemap\.org\/|\/basemap\/[^/]+\/style\.json$/, (route) =>
          route.fulfill({
            json: {
              version: 8,
              sources: {},
              layers: [{ id: "bg", type: "background", paint: { "background-color": "#e8e4dc" } }],
            },
          }),
        );
        await page.goto(`/?q=%22cross+of+gold%22&bucket=month&c=-89,40&z=5${direct ? "&layer=states" : ""}`);
        await expect(page.getByTestId("map")).toBeVisible();
        let beforeVariants = 0;
        if (!direct) {
          // Also exercise an already-ready basemap, without prefetching States.
          await page.waitForTimeout(500);
          await expect(page.locator("#search-overlay")).toHaveCSS("opacity", "1");
          beforeVariants = await page.evaluate(
            () => performance.getEntriesByName("states-diagnostic-shader-variant").length,
          );
          expect(
            await page.evaluate(() =>
              performance.getEntriesByType("resource").some((e) => e.name.endsWith(".geojson")),
            ),
          ).toBe(false);
          await page.getByRole("combobox", { name: "Map layer" }).selectOption("states");
        }
        await page.waitForFunction(() => performance.getEntriesByName("usnm-states-first-paint").length > 0);
        // Let already-swapped frame images reach the controller. Use CDP's frame
        // swap timestamp, not callback delivery or screenshot-tool latency.
        await page.waitForTimeout(100);
        await cdp.send("Page.stopScreencast");
        const visibleProof = await page.evaluate(async (frames) => {
          const asset = performance
            .getEntriesByType("resource")
            .find((e) => e.name.endsWith(".geojson")) as PerformanceResourceTiming;
          const arrivedAt = performance.timeOrigin + asset.responseEnd;
          const palette = Array.from(document.querySelectorAll(".states-legend li:nth-child(-n+5) .states-swatch")).map(
            (swatch) =>
              getComputedStyle(swatch)
                .backgroundColor.match(/\d+/g)!
                .slice(0, 3)
                .map((value, i) => Math.round((Number(value) * 145 + [232, 228, 220][i]! * 110) / 255)),
          );
          const bounds = document.querySelector(".maplibregl-canvas")!.getBoundingClientRect();
          const x = bounds.left + bounds.width / 2;
          const y = bounds.top + bounds.height / 2;
          if (x >= innerWidth || y >= innerHeight) throw new Error("Illinois proof pixel is outside the viewport");
          for (const frame of frames) {
            if (frame.timestamp < arrivedAt) continue;
            const bytes = Uint8Array.from(atob(frame.data), (character) => character.charCodeAt(0));
            const image = await createImageBitmap(new Blob([bytes], { type: "image/png" }));
            const canvas = new OffscreenCanvas(image.width, image.height);
            const context = canvas.getContext("2d")!;
            context.drawImage(image, 0, 0);
            const pixel = Array.from(
              context.getImageData(
                Math.floor((x * image.width) / innerWidth),
                Math.floor((y * image.height) / innerHeight),
                1,
                1,
              ).data,
            );
            image.close();
            // The fixture basemap is plain #e8e4dc; the centre is Illinois,
            // away from its border, labels, controls and the separate deck canvas.
            if (
              pixel[3] === 255 &&
              palette.some((color) => color.every((value, i) => Math.abs(value - pixel[i]!) <= 2))
            )
              return { duration: frame.timestamp - arrivedAt, pixel };
          }
          return null;
        }, frames);
        expect(visibleProof, "a composited frame must contain the Illinois state fill").not.toBeNull();
        visibleProofSamples.push(visibleProof!.duration);
        visiblePixels.push(visibleProof!.pixel);
        samples.push(await page.evaluate(() => performance.getEntriesByName("usnm-states-first-paint")[0]!.duration));
        drawSamples.push(
          await page.evaluate(() => performance.getEntriesByName("usnm-states-first-draw")[0]!.duration),
        );
        parseSamples.push(
          await page.evaluate(() => {
            const resource = performance
              .getEntriesByType("resource")
              .find((e) => e.name.endsWith(".geojson")) as PerformanceResourceTiming;
            return performance.getEntriesByName("usnm-states-parsed")[0]!.startTime - resource.responseEnd;
          }),
        );
        shaderVariants.push(
          (await page.evaluate(() => performance.getEntriesByName("states-diagnostic-shader-variant").length)) -
            beforeVariants,
        );
        shaderStalls.push(
          await page.evaluate(() => {
            const asset = performance
              .getEntriesByType("resource")
              .find((e) => e.name.endsWith(".geojson")) as PerformanceResourceTiming;
            const calls = performance
              .getEntriesByType("measure")
              .filter((e) => e.name.startsWith("states-diagnostic-gl-") && e.startTime >= asset.responseEnd)
              .map((e) => ({
                name: e.name.replace("states-diagnostic-gl-", ""),
                duration: e.duration,
                stack: (e as PerformanceMeasure).detail?.stack as string | undefined,
              }));
            return {
              totalMs: calls.reduce((sum, call) => sum + call.duration, 0),
              maxMs: Math.max(0, ...calls.map((call) => call.duration)),
              calls: calls.filter((call) => call.duration >= 1),
            };
          }),
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
        kind: "asset responseEnd to native render/two-frame proxy and composited Illinois fill frame swap",
        entryMode: direct ? "direct URL" : "layer selection",
        host: { cpu: cpus()[0]?.model, platform: platform(), arch: arch() },
        browserName,
        cacheScope: "fresh browser processes; Chrome GPU caches disabled; OS/driver cache uncontrolled",
        shaderCold,
        shaderVariants,
        shaderStalls,
        userAgent,
        renderer,
        samplesMs: samples,
        visibleProofSamplesMs: visibleProofSamples,
        visiblePixels,
        drawSamplesMs: drawSamples,
        parseSamplesMs: parseSamples,
        maxMs: Math.max(...samples),
      }),
    );
    if (shaderCold) expect(shaderVariants.every((count) => count >= 3)).toBe(true);
    expect(Math.max(...samples)).toBeLessThanOrEqual(300);
    expect(Math.max(...visibleProofSamples)).toBeLessThanOrEqual(300);
  });
}
