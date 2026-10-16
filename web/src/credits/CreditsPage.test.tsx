import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import CreditsPage from "./CreditsPage";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});
function renderPage(sources: unknown) {
  vi.stubGlobal(
    "fetch",
    vi.fn(
      async () =>
        new Response(JSON.stringify({ schema: 1, published: { index_version: "serving-v1", sources } }), {
          headers: { "content-type": "application/json" },
        }),
    ),
  );
  return render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <CreditsPage />
    </QueryClientProvider>,
  );
}
describe("OCR credits page", () => {
  it("renders only serving engines and their selected models", async () => {
    renderPage({
      ja_latin: { pages: 3, by_engine: [{ engine: "ndlocr-lite 636d1cf", pages: 3 }] },
      ocr_local: { by_engine: [{ engine: "tesseract/5.5.1 frak2021", pages: 2 }] },
    });
    await screen.findByText("Serving version: serving-v1.");
    expect(screen.getByRole("heading", { name: "ndlocr-lite 636d1cf" }).parentElement?.id).toBe("ndlocr-lite 636d1cf");
    expect(screen.getByRole("link", { name: "DINOv3 License" }).getAttribute("href")).toContain("dinov3");
    expect(screen.getByText("National Diet Library")).toBeTruthy();
    expect(screen.getByText("Stefan Weil, UB Mannheim")).toBeTruthy();
    expect(screen.getByText(/Japanese-language newspapers whose/)).toBeTruthy();
    expect(screen.getByText(/Language: German/)).toBeTruthy();
    expect(screen.queryByText(/MinerU/)).toBeNull();
    expect(screen.queryByText(/tessdata\/eng.traineddata/)).toBeNull();
  });
  it("credits Japanese-only search pages even without Latin replacements", async () => {
    renderPage({
      ja_latin: null,
      ocr_local: null,
      ocr_ja: { pages: 4, by_engine: [{ engine: "ndlocr-lite 636d1cf", pages: 4 }] },
    });
    await screen.findByRole("heading", { name: "ndlocr-lite 636d1cf" });
    expect(screen.getByText(/4 pages/)).toBeTruthy();
    expect(screen.queryByText(/no pages with our replacement OCR/)).toBeNull();
  });

  it("spells out every language in the Bosnian, Norwegian and Ancient Greek profiles", async () => {
    renderPage({
      ja_latin: null,
      ocr_local: {
        by_engine: [
          { engine: "tesseract/5.5.1 hrv", pages: 1 },
          { engine: "tesseract/5.5.1 nor", pages: 1 },
          { engine: "tesseract/5.5.1 grc", pages: 1 },
        ],
      },
    });
    await screen.findByRole("heading", { name: "tesseract/5.5.1 hrv" });
    expect(screen.getByText(/Language: Croatian, Bosnian/)).toBeTruthy();
    expect(screen.getByText(/Norwegian Bokmål, Norwegian Nynorsk/)).toBeTruthy();
    expect(screen.getByText(/Ancient Greek/)).toBeTruthy();
    expect(screen.queryByText(/Language:.*\b(?:Bos|Nob|Nno|Grc)\b/)).toBeNull();
  });

  it("spells out the new French and Slovak ISO language codes", async () => {
    renderPage({
      ocr_local: {
        by_engine: [
          { engine: "tesseract/5.5.1 fra", pages: 1 },
          { engine: "tesseract/5.5.1 slk", pages: 1 },
        ],
      },
    });
    await screen.findByRole("heading", { name: "tesseract/5.5.1 fra" });
    expect(screen.getByText(/Language: French/)).toBeTruthy();
    expect(screen.getByText(/Language: Slovak/)).toBeTruthy();
    expect(screen.queryByText(/Language: (Fra|Slk)\./)).toBeNull();
  });

  it("reports an unknown serving engine instead of dropping it", async () => {
    renderPage({ ocr_local: { by_engine: [{ engine: "uncredited/1 eng", pages: 1 }] } });
    expect((await screen.findByRole("alert")).textContent).toContain("Missing OCR credit: uncredited/1 eng");
  });
  it("does not imply unused engines are in production", async () => {
    renderPage({ ja_latin: null, ocr_local: null });
    await screen.findByText("The serving version has no pages with our replacement OCR text.");
    expect(screen.queryByRole("heading", { level: 2 })).toBeNull();
  });
  it("reports missing serving inventory on old servers", async () => {
    renderPage(undefined);
    expect((await screen.findByRole("alert")).textContent).toContain("does not provide");
  });
});
