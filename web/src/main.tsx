import { lazy, StrictMode, Suspense } from "react";
import { createRoot } from "react-dom/client";
import { QueryCache, QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { App } from "./App";
import { isRetryable, VersionChangedError } from "./api/client";
import { NotFound } from "./components/NotFound";
import { trackPageView } from "./pageview";
import { DATASETS_PATH, OLD_DATASETS_PATH, pageFor } from "./route";
import "./styles.css";
import { preloadBasemap } from "./lib/preloadBasemap";
import { enhanceSearchLanding } from "./searchLanding";
import type { Meta } from "./api/types";

// The dataset details, privacy and credits pages load only when visited.
// The API serves index.html for app paths, so all work as direct links;
// other paths get it with a 404, and the
// not-found page.
const DatasetsPage = lazy(() => import("./status/StatusPage"));
const CreditsPage = lazy(() => import("./credits/CreditsPage"));
const PrivacyPage = lazy(() => import("./privacy/PrivacyPage"));
const searchNavigation = document.getElementById("common-searches");
const searchLinks = searchNavigation
  ? [...searchNavigation.querySelectorAll<HTMLAnchorElement>("a")].map((link) => ({
      href: link.getAttribute("href")!,
      label: link.querySelector("strong")?.textContent ?? link.textContent ?? "",
    }))
  : undefined;
const page = pageFor(window.location.pathname);
// The old path, where a server didn't redirect it: show the new one.
if (page === "datasets" && window.location.pathname.replace(/\/+$/, "") === OLD_DATASETS_PATH) {
  window.history.replaceState(null, "", `${DATASETS_PATH}${window.location.search}${window.location.hash}`);
}
// One page view per page shown; each page is a page load (no client router).
trackPageView(page);

const client: QueryClient = new QueryClient({
  // A new index version was published mid-session: refetch /v1/meta. Every
  // version-scoped query key includes the version, so they all move to the
  // new snapshot together instead of mixing old and new responses.
  queryCache: new QueryCache({
    onError: (err) => {
      if (err instanceof VersionChangedError) void client.invalidateQueries({ queryKey: ["meta"] });
    },
  }),
  defaultOptions: {
    queries: {
      staleTime: 5 * 60_000,
      // Problems (4xx), version changes, and searches that ran out of
      // time or found the API busy after waiting are not retried.
      retry: (n, err) => isRetryable(err) && n < 2,
      refetchOnWindowFocus: false,
    },
  },
});

const initialMeta = document.getElementById("site-meta")?.textContent;
if (initialMeta) client.setQueryData<Meta>(["meta"], JSON.parse(initialMeta));

function mount() {
  if (page === "search" && !("__USNM_SHARE_CAPTURE__" in window)) void preloadBasemap();
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <QueryClientProvider client={client}>
        {page === "datasets" ? (
          <Suspense fallback={<p role="status">Loading…</p>}>
            <DatasetsPage />
          </Suspense>
        ) : page === "credits" ? (
          <Suspense fallback={<p role="status">Loading…</p>}>
            <CreditsPage />
          </Suspense>
        ) : page === "privacy" ? (
          <Suspense fallback={<p role="status">Loading…</p>}>
            <PrivacyPage />
          </Suspense>
        ) : page === "not-found" ? (
          <NotFound />
        ) : (
          <App searchLinks={searchLinks} />
        )}
      </QueryClientProvider>
    </StrictMode>,
  );
}

if ("__USNM_SHARE_CAPTURE__" in window) {
  document.getElementById("search-landing")?.remove();
  mount();
} else if (!enhanceSearchLanding(mount)) {
  mount();
}
