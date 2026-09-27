import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { App } from "./App";
import { ApiError } from "./api/client";
import "./styles.css";

const client = new QueryClient({
  defaultOptions: {
    queries: {
      // Versioned responses never change; problems (4xx) are not retried.
      staleTime: 5 * 60_000,
      retry: (n, err) => !(err instanceof ApiError && err.problem.status < 500) && n < 2,
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
