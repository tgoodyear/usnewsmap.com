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
