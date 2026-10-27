import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ShareButton } from "./ShareButton";

const image = "/share-image.png?page=%2F&q=cross+of+gold&state=IL&v=2";
const metadata = () =>
  new Response(`<meta property="og:image" content="https://usnewsmap.com${image.replaceAll("&", "&amp;")}">`);
const png = (fallback = false) =>
  new Response("png", {
    headers: { "content-type": "image/png", "cache-control": fallback ? "no-store" : "max-age=3600" },
  });

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  window.history.replaceState(null, "", "/");
});

describe("Share preview preparation", () => {
  it("only starts on click, copies the current SPA URL immediately and uses fresh decoded metadata", async () => {
    const copy = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText: copy } });
    let release!: (response: Response) => void;
    const fetch = vi
      .fn()
      .mockImplementationOnce(() => new Promise<Response>((resolve) => (release = resolve)))
      .mockResolvedValueOnce(png());
    vi.stubGlobal("fetch", fetch);
    render(<ShareButton />);
    window.history.replaceState(null, "", "/search/cross-of-gold?state=IL");
    expect(fetch).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Share" }));
    expect(copy).toHaveBeenCalledWith(window.location.href);
    expect(fetch).toHaveBeenCalledWith(window.location.href, { signal: expect.any(AbortSignal) });
    await waitFor(() => expect(screen.getByRole("button").textContent).toBe("Link copied"));
    expect(screen.getByRole("status").textContent).toBe("Preparing preview…");
    fireEvent.click(screen.getByRole("button"));
    expect(fetch).toHaveBeenCalledTimes(1);
    await act(async () => release(metadata()));
    await waitFor(() => expect(screen.getByRole("status").textContent).toBe("Preview ready for copied link"));
    expect(fetch).toHaveBeenLastCalledWith(new URL(image, window.location.href).href, {
      signal: expect.any(AbortSignal),
    });
  });

  it("waits past a static fallback and bounds preparation when rendering never completes", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("navigator", { clipboard: { writeText: vi.fn().mockResolvedValue(undefined) } });
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(metadata())
      .mockImplementation(async (_url, { signal }) => {
        signal.throwIfAborted();
        return png(true);
      });
    vi.stubGlobal("fetch", fetch);
    render(<ShareButton />);
    await act(async () => fireEvent.click(screen.getByRole("button")));
    expect(screen.getByRole("status").textContent).toBe("Preparing preview…");
    await act(async () => vi.advanceTimersByTimeAsync(48_000));
    expect(screen.getByRole("status").textContent).toContain("Preview unavailable");
    expect(fetch.mock.calls.length).toBeLessThan(25);
  });

  it("only reports readiness after a fallback is replaced by a completed screenshot", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("navigator", { clipboard: { writeText: vi.fn().mockResolvedValue(undefined) } });
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(metadata())
      .mockResolvedValueOnce(png(true))
      .mockResolvedValueOnce(png());
    vi.stubGlobal("fetch", fetch);
    render(<ShareButton />);
    await act(async () => fireEvent.click(screen.getByRole("button")));
    expect(screen.getByRole("status").textContent).toBe("Preparing preview…");
    await act(async () => vi.advanceTimersByTimeAsync(2000));
    expect(screen.getByRole("status").textContent).toBe("Preview ready for copied link");
    expect(fetch).toHaveBeenCalledTimes(3);
  });

  it("reports copying and preview failures without losing the usable link", async () => {
    vi.stubGlobal("navigator", { clipboard: { writeText: vi.fn().mockRejectedValue(new Error("denied")) } });
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("unavailable", { status: 503 })));
    render(<ShareButton />);
    fireEvent.click(screen.getByRole("button"));
    await waitFor(() => expect(screen.getByRole("status").textContent).toContain("Preview unavailable"));
    expect(screen.getByRole("status").textContent).toContain("Copy failed");
    expect(screen.getByRole("button").hasAttribute("disabled")).toBe(false);
  });

  it("does not fetch an external image advertised by unexpected metadata", async () => {
    vi.stubGlobal("navigator", { clipboard: { writeText: vi.fn().mockResolvedValue(undefined) } });
    const fetch = vi
      .fn()
      .mockResolvedValue(new Response('<meta property="og:image" content="https://example.com/image.png">'));
    vi.stubGlobal("fetch", fetch);
    render(<ShareButton />);
    fireEvent.click(screen.getByRole("button"));
    await waitFor(() => expect(screen.getByRole("status").textContent).toContain("Preview unavailable"));
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("cancels preparation when the sharing control unmounts", async () => {
    vi.stubGlobal("navigator", { clipboard: { writeText: vi.fn().mockResolvedValue(undefined) } });
    let signal!: AbortSignal;
    const fetch = vi.fn().mockImplementation((_url, options) => {
      signal = options.signal;
      return new Promise((_resolve, reject) => {
        signal.addEventListener("abort", () => reject(signal.reason), { once: true });
      });
    });
    vi.stubGlobal("fetch", fetch);
    const { rerender } = render(<ShareButton key="old-view" />);
    fireEvent.click(screen.getByRole("button"));
    expect(screen.getByRole("status").textContent).toBe("Preparing preview…");
    await act(async () => rerender(<ShareButton key="new-view" />));
    expect(signal.aborted).toBe(true);
    expect(screen.getByRole("status").textContent).toBe("");
    expect(fetch).toHaveBeenCalledTimes(1);
  });
});
