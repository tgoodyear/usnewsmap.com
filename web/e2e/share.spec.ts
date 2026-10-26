import { expect, test } from "@playwright/test";

test("search previews are rendered PNGs of the linked view and cached", async ({ request, isMobile }) => {
  test.skip(!process.env.PW_BASE_URL || isMobile, "requires the API serving the built app and Chromium");
  test.setTimeout(90_000);
  const search = "q=cross+of+gold&from=1896-01-01&to=1896-12-31";
  const html = await (await request.get(`/?${search}`)).text();
  const image = /property="og:image" content="([^"]+)"/.exec(html)?.[1];
  expect(image).toContain("/share-image.png?");
  const imagePath = new URL(image!.replaceAll("&amp;", "&")).pathname + new URL(image!.replaceAll("&amp;", "&")).search;
  const first = await request.get(imagePath, { timeout: 40_000 });
  expect(first.status()).toBe(200);
  expect(first.headers()["content-type"]).toBe("image/png");
  const png = await first.body();
  expect(png.subarray(0, 8)).toEqual(Buffer.from("\x89PNG\r\n\x1a\n", "binary"));
  expect(png.readUInt32BE(16)).toBe(1200);
  expect(png.readUInt32BE(20)).toBe(630);
  expect(await (await request.get(imagePath)).body()).toEqual(png);
  const filtered = await request.get(`${imagePath}&state=GA&t=1896-08-01`, { timeout: 40_000 });
  expect(filtered.status()).toBe(200);
  expect(await filtered.body()).not.toEqual(png);
});
