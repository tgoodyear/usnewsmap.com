import { expect, test, type Page } from "@playwright/test";

// Map gestures against the API serving the synthetic fixtures.

const STYLE = {
  version: 8,
  sources: {},
  layers: [{ id: "bg", type: "background", paint: { "background-color": "#e8e4dc" } }],
};

test.beforeEach(async ({ page }) => {
  await page.route("https://tiles.openfreemap.org/**", (route) =>
    route.request().url().includes("/styles/") ? route.fulfill({ json: STYLE }) : route.fulfill({ status: 404 }),
  );
});

/** The zoom the URL records, written when a map move ends. */
const urlZoom = (page: Page) => Number(new URL(page.url()).searchParams.get("z"));

/** A two-finger pinch outward from the map's centre, sent as real touch events. */
async function pinchOut(page: Page) {
  const box = (await page.getByTestId("map").boundingBox())!;
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  const cdp = await page.context().newCDPSession(page);
  const touch = (type: "touchStart" | "touchMove" | "touchEnd", d: number) =>
    cdp.send("Input.dispatchTouchEvent", {
      type,
      touchPoints:
        type === "touchEnd"
          ? []
          : [
              { x: x - d, y, id: 0 },
              { x: x + d, y, id: 1 },
            ],
    });
  await touch("touchStart", 20);
  // Slow enough that playback renders many frames while the fingers move.
  for (let d = 30; d <= 140; d += 10) {
    await touch("touchMove", d);
    await page.waitForTimeout(50);
  }
  await touch("touchEnd", 140);
}

test("pinch zoom works while the timeline plays", async ({ page, isMobile }) => {
  test.skip(!isMobile, "touch gestures: the mobile project");
  // A day per step over a year, so playback runs well past the gesture.
  await page.goto(
    "/?q=%22cross+of+gold%22&from=1896-01-01&to=1896-12-31&bucket=day&t=1896-01-01&z=4.00&c=-96.000,38.500",
  );
  await expect(page.locator(".summary")).toContainText("places");
  await expect(page.locator(".maplibregl-canvas")).toBeVisible();

  const play = page.getByRole("button", { name: /Play|Pause/ });
  await play.click();
  await expect(play).toHaveAttribute("aria-pressed", "true");

  await pinchOut(page);
  // Still playing, so every frame of the gesture raced a re-render.
  await expect(play).toHaveAttribute("aria-pressed", "true");
  // Fingers 40 px apart spread to 280 px: about 2.8 zoom levels in.
  await expect.poll(() => urlZoom(page)).toBeGreaterThan(5);
});

test("phones put the playback controls between the map and the legend, in reading order", async ({
  page,
  isMobile,
}) => {
  await page.goto("/?q=%22cross+of+gold%22&from=1896-06-01&to=1896-12-31&bucket=week");
  await expect(page.locator(".summary")).toContainText("places");
  const order = () =>
    page.evaluate(() => {
      const pick = (sel: string) => document.querySelector(sel);
      const [map, bar, legend] = [pick(".map-wrap"), pick(".timebar"), pick(".legend")];
      if (!map || !bar || !legend) return "missing";
      const before = (a: Element, b: Element) => !!(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING);
      return before(map, bar) && before(bar, legend) ? "map, controls, legend" : "map, legend, controls";
    });
  // DOM order, which keyboard and screen-reader users follow, matches what each layout shows.
  await expect.poll(order).toBe(isMobile ? "map, controls, legend" : "map, legend, controls");
  await expect(page.locator(".timebar")).toHaveCount(1);
  if (isMobile) return;
  // Crossing the breakpoint moves the legend and lists, never the controls, so playback keeps going.
  const play = page.getByRole("button", { name: /Play|Pause/ });
  await play.click();
  await expect(play).toHaveAttribute("aria-pressed", "true");
  await page.setViewportSize({ width: 400, height: 800 });
  await expect.poll(order).toBe("map, controls, legend");
  await expect(play).toHaveAttribute("aria-pressed", "true");
});

test("the overlay waits for the basemap, then fades in", async ({ page }) => {
  await page.route("https://tiles.openfreemap.org/styles/positron", async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 1000));
    await route.fulfill({ json: STYLE });
  });
  await page.goto("/?q=%22cross+of+gold%22&from=1896-06-01&to=1896-12-31");
  const overlay = page.locator("#search-overlay");
  await expect(overlay).toHaveCSS("opacity", "0");
  await expect(page.getByTestId("map")).toHaveClass(/map--basemap-ready/);
  await expect(overlay).toHaveCSS("opacity", "1");
  await expect(overlay).toHaveCSS("transition-duration", "0.18s");
});

test("a blocked basemap does not hide results indefinitely, including with reduced motion", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.route("https://tiles.openfreemap.org/**", (route) => route.abort());
  await page.goto("/?q=%22cross+of+gold%22&from=1896-06-01&to=1896-12-31");
  await expect(page.locator(".summary")).toContainText("places");
  const overlay = page.locator("#search-overlay");
  await expect(overlay).toHaveCSS("opacity", "1");
  await expect(overlay).toHaveCSS("transition-duration", "0s");
});

for (const direct of [false, true]) {
  for (const failure of ["abort", "timeout"]) {
    test(`States survives ${failure} of the basemap style (${direct ? "direct entry" : "switch from Points"})`, async ({
      page,
      isMobile,
    }) => {
      await page.route("https://tiles.openfreemap.org/**", async (route) => {
        if (failure === "timeout") await new Promise((resolve) => setTimeout(resolve, 3000));
        await route.abort();
      });
      await page.goto(
        `/?q=%22cross+of+gold%22&from=1896-06-01&to=1896-12-31&c=-89,40&z=5${direct ? "&layer=states" : ""}`,
      );
      if (!direct) await page.getByRole("combobox", { name: "Map layer" }).selectOption("states");
      await page.waitForFunction(() => performance.getEntriesByName("usnm-states-first-paint").length > 0);
      await expect(page.getByRole("region", { name: "States legend" })).toBeVisible();
      const map = page.getByTestId("map");
      await map.evaluate((element) => element.scrollIntoView({ block: "center", behavior: "instant" }));
      const box = (await map.boundingBox())!;
      const [x, y] = [box.x + box.width / 2, box.y + box.height / 2];
      if (isMobile) {
        await page.touchscreen.tap(x, y);
        await expect(page.getByRole("group", { name: "Illinois details" })).toContainText("pages published");
      } else {
        await page.mouse.move(x, y);
        await expect(page.getByRole("tooltip").filter({ hasText: "Illinois" })).toContainText("pages published");
      }
    });
  }
}
