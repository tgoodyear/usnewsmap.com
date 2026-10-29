/** Keep the static map visible until the equivalent interactive view has painted. */
export function enhanceSearchLanding(mount: () => void): boolean {
  const landing = document.getElementById("search-landing");
  const root = document.getElementById("root");
  if (!landing || !root) return false;
  root.classList.add("search-hydrating");
  root.setAttribute("aria-hidden", "true");
  root.inert = true;
  const status = document.createElement("p");
  status.className = "empty";
  status.setAttribute("role", "status");
  status.textContent = "Loading interactive map. The static map and newspaper results are available below.";
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
  let presenting = false;
  const observer = new MutationObserver(() => {
    if (!ready()) {
      if (root.querySelector('[role="alert"], [data-basemap-error="true"]')) {
        status.textContent =
          "The interactive map is unavailable. The static map and newspaper results remain available.";
      }
      return;
    }
    if (presenting) return;
    presenting = true;
    requestAnimationFrame(() =>
      requestAnimationFrame(() => {
        if (!ready()) {
          presenting = false;
          return;
        }
        const hadFocus = landing.contains(document.activeElement);
        landing.remove();
        root.classList.remove("search-hydrating");
        root.removeAttribute("aria-hidden");
        root.inert = false;
        if (hadFocus) root.querySelector<HTMLInputElement>('input[type="search"]')?.focus();
        observer.disconnect();
      }),
    );
  });
  observer.observe(root, { subtree: true, childList: true, attributes: true });
  mount();
  return true;
}
