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
  await page.goto("/?q=%22cross+of+gold%22&from=1896-01-01&to=1896-12-31&bucket=day&t=1896-01-01&z=4.00&c=-96.000,38.500");
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
