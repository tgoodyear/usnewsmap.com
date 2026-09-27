import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryCache, QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { App } from "./App";
import { ApiError, VersionChangedError } from "./api/client";
import "./styles.css";

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
      <App />
    </QueryClientProvider>
  </StrictMode>,
);
