import { parseViewUrl, searchParams } from "./state/url";

function searchIdentity(): string {
  const params = searchParams(parseViewUrl(window.location.pathname + window.location.search));
  return JSON.stringify({ ...params, bucket: undefined });
}

/** Keep server-rendered results outside React, even when the API is unavailable. */
export function enhanceSearchLanding(mount: () => void): boolean {
  const landing = document.getElementById("search-landing");
  const button = landing?.querySelector<HTMLButtonElement>("#explore-map");
  if (!landing || !button) return false;
  const identity = searchIdentity();
  window.addEventListener("popstate", () => {
    landing.hidden = searchIdentity() !== identity;
  });
  button.hidden = false;
  button.addEventListener(
    "click",
    () => {
      button.hidden = true;
      mount();
    },
    { once: true },
  );
  return true;
}

export function focusInteractiveSearch(): void {
  const landingMain = document.querySelector("#search-landing main");
  if (!landingMain) return;
  const region = document.createElement("section");
  region.className = landingMain.className;
  region.setAttribute("aria-label", "Original newspaper search results");
  region.append(...landingMain.childNodes);
  landingMain.replaceWith(region);
  const footer = document.querySelector<HTMLElement>("#search-landing footer");
  if (footer) footer.hidden = true;
  if ("__USNM_SHARE_CAPTURE__" in window) return;
  const search = document.querySelector<HTMLInputElement>('#root input[type="search"]');
  search?.focus();
}
