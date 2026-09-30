import { lazy, StrictMode, Suspense } from "react";
import { createRoot } from "react-dom/client";
import { QueryCache, QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { App } from "./App";
import { ApiError, VersionChangedError } from "./api/client";
import { NotFound } from "./components/NotFound";
import { trackPageView } from "./pageview";
import { pageFor } from "./route";
import "./styles.css";

// The status and privacy pages are their own routes, loaded only when
// visited. The API serves index.html for app paths, so /status and /privacy
// work as direct links; other paths get it with a 404, and the not-found page.
const StatusPage = lazy(() => import("./status/StatusPage"));
const PrivacyPage = lazy(() => import("./privacy/PrivacyPage"));
const page = pageFor(window.location.pathname);
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
      // Problems (4xx) and version changes are not retried.
      retry: (n, err) =>
        !(err instanceof VersionChangedError) &&
        !(err instanceof ApiError && err.problem.status < 500) &&
        n < 2,
      refetchOnWindowFocus: false,
    },
  },
});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={client}>
      {page === "status" ? (
        <Suspense fallback={<p role="status">Loading…</p>}>
          <StatusPage />
        </Suspense>
      ) : page === "privacy" ? (
        <Suspense fallback={<p role="status">Loading…</p>}>
          <PrivacyPage />
        </Suspense>
      ) : page === "not-found" ? (
        <NotFound />
      ) : (
        <App />
      )}
    </QueryClientProvider>
  </StrictMode>,
);
