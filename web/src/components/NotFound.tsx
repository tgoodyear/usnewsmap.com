import { useEffect } from "react";
import { TITLES } from "../route";

/**
 * A path the app has no page for. The API sends the app shell with a 404 for
 * these, so this is what a visitor sees.
 */
export function NotFound() {
  useEffect(() => {
    document.title = TITLES["not-found"];
  }, []);
  return (
    <div className="app">
      <header className="topbar">
        <a className="brand" href="/" aria-label="US News Map home">
          <span aria-hidden="true">◉</span> US News Map
        </a>
      </header>
      <main className="empty">
        <h1>Page not found</h1>
        <p>
          There is no page at this address. <a href="/">Search the newspapers</a> from the home page.
        </p>
      </main>
      <footer className="credits">
        Newspaper pages from{" "}
        <a href="https://chroniclingamerica.loc.gov/" target="_blank" rel="noopener noreferrer">
          Chronicling America
        </a>{" "}
        (
        <a href="https://www.loc.gov/" target="_blank" rel="noopener noreferrer">
          Library of Congress
        </a>
        ). <a href="/privacy">Privacy</a>
      </footer>
    </div>
  );
}
