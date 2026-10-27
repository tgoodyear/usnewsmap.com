import { useEffect, useRef, useState } from "react";

async function preparePreview(link: string, signal: AbortSignal) {
  // Read fresh server metadata: the SPA's initial tags may describe another view.
  const page = await fetch(link, { signal });
  if (!page.ok) throw new Error("Could not load sharing metadata");
  const html = new DOMParser().parseFromString(await page.text(), "text/html");
  const image = html.querySelector('meta[property="og:image"]')?.getAttribute("content");
  if (!image) throw new Error("Sharing metadata has no preview");
  const advertised = new URL(image, link);
  if (advertised.pathname !== "/share-image.png") {
    throw new Error("Sharing metadata has no dynamic preview");
  }
  // Metadata has the production origin even on local/preview deployments.
  const url = new URL(advertised.pathname + advertised.search, link);
  for (;;) {
    signal.throwIfAborted();
    const response = await fetch(url.href, { signal });
    if (!response.ok || response.headers.get("content-type")?.split(";")[0] !== "image/png") {
      throw new Error("Could not prepare sharing preview");
    }
    await response.arrayBuffer();
    // The safe timeout image is not the completed screenshot.
    if (!response.headers.get("cache-control")?.includes("no-store")) return;
    await new Promise((resolve) => setTimeout(resolve, 2000));
  }
}

export function ShareButton() {
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState(false);
  const [preview, setPreview] = useState<"idle" | "preparing" | "ready" | "error">("idle");
  const active = useRef<AbortController | null>(null);
  useEffect(
    () => () => {
      active.current?.abort();
      active.current = null;
    },
    [],
  );

  const share = () => {
    if (active.current) return;
    const link = window.location.href;
    setCopied(false);
    setCopyError(false);
    // Keep clipboard access inside the click's user activation.
    if (navigator.clipboard) {
      void navigator.clipboard.writeText(link).then(
        () => setCopied(true),
        () => setCopyError(true),
      );
    } else {
      setCopyError(true);
    }
    const controller = new AbortController();
    active.current = controller;
    setPreview("preparing");
    const timeout = setTimeout(() => controller.abort(), 45_000);
    void preparePreview(link, controller.signal)
      .then(
        () => setPreview("ready"),
        () => {
          if (active.current === controller) setPreview("error");
        },
      )
      .finally(() => {
        clearTimeout(timeout);
        if (active.current === controller) active.current = null;
      });
  };

  return (
    <div className="toolbar__share">
      <button type="button" className="button" disabled={preview === "preparing"} onClick={share}>
        {copied ? "Link copied" : "Share"}
      </button>
      <span className="share-status" role="status">
        {copyError && "Copy failed. Copy the address bar link. "}
        {preview === "preparing" && "Preparing preview…"}
        {preview === "ready" && (copied ? "Preview ready for copied link" : "Preview ready")}
        {preview === "error" && "Preview unavailable. The link still works."}
      </span>
    </div>
  );
}
