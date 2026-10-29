/** Show the app immediately, retaining static results only as an error fallback. */
export function enhanceSearchLanding(mount: () => void): boolean {
  const landing = document.getElementById("search-landing");
  const root = document.getElementById("root");
  if (!landing || !root) return false;
  landing.hidden = true;
  const status = document.createElement("p");
  status.className = "empty";
  status.setAttribute("role", "status");
  status.textContent = "The interactive map is unavailable. The static map and newspaper results remain available.";
  landing.prepend(status);

  const ready = () => {
    const results = root.querySelector('[data-share-ready="true"]');
    const map = results?.querySelector('[data-testid="map"]');
    const painted =
      map?.getAttribute("data-basemap-ready") === "true" &&
      map.getAttribute("data-overlay-ready") === "true" &&
      map.getAttribute("data-basemap-error") !== "true";
    const empty =
      results && [...results.querySelectorAll(".notice strong")].some((n) => n.textContent === "No pages match.");
    return !!results && (painted || !!empty || !!results.querySelector(".tables"));
  };
  const discard = () => {
    const hadFocus = landing.contains(document.activeElement);
    landing.remove();
    root.hidden = false;
    if (hadFocus) root.querySelector<HTMLInputElement>('input[type="search"]')?.focus();
    observer.disconnect();
    window.removeEventListener("popstate", discard);
  };
  const observer = new MutationObserver(() => {
    if (ready()) {
      discard();
      return;
    }
    const failed = !!root.querySelector('[role="alert"], [data-basemap-error="true"]');
    if (root.hidden !== failed) root.hidden = failed;
    landing.classList.toggle("search-static-fallback", failed);
    landing.hidden = !failed;
  });
  observer.observe(root, { subtree: true, childList: true, attributes: true });
  window.addEventListener("popstate", discard);
  mount();
  return true;
}
