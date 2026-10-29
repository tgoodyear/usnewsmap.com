/** Keep server-rendered results outside React, even when the API is unavailable. */
export function enhanceSearchLanding(mount: () => void): boolean {
  const landing = document.getElementById("search-landing");
  const button = landing?.querySelector<HTMLButtonElement>("#explore-map");
  if (!landing || !button) return false;
  button.hidden = false;
  button.addEventListener(
    "click",
    () => {
      button.hidden = true;
      mount();
      document.getElementById("root")?.scrollIntoView();
    },
    { once: true },
  );
  return true;
}
