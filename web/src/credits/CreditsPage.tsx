import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../api/client";
import { Brand } from "../components/Brand";
import { TITLES } from "../route";
import { languageName } from "../lib/languages";
import { count } from "../status/format";
import { catalog, licenseHref, servingCredits, type Attribution, type UsedCredit } from "./data";

function Credit({ entry, name, required }: { entry: Attribution; name: string; required: string[] }) {
  const href = licenseHref(entry);
  return (
    <div className="ocr-credit">
      <h3>{name}</h3>
      <p>{entry.credit}</p>
      <dl>
        <dt>Author / publisher</dt>
        <dd>{entry.author}</dd>
        <dt>Source</dt>
        <dd>
          <a href={entry.source}>{entry.source}</a>
        </dd>
        <dt>Licence</dt>
        <dd>
          {href ? (
            <a href={href}>{entry.license === "LicenseRef-DINOv3" ? "DINOv3 License" : entry.license}</a>
          ) : entry.license === "none" ? (
            "no explicit licence"
          ) : (
            entry.license
          )}
          {required.includes(entry.license) && " (attribution required)"}
        </dd>
        {entry.fetched && (
          <>
            <dt>Source checked / fetched</dt>
            <dd>{String(entry.fetched).slice(0, 10)}</dd>
          </>
        )}
      </dl>
      {entry.note && <p>{entry.note}</p>}
    </div>
  );
}

export default function CreditsPage() {
  useEffect(() => {
    document.title = TITLES.credits;
  }, []);
  const status = useQuery({ queryKey: ["status"], queryFn: ({ signal }) => api.status(signal), staleTime: 0 });
  const sources = status.data?.published.sources;
  const required = sources?.ocr_credits?.credit_required ?? catalog.credit_required;
  let credits: UsedCredit[] = [];
  let missing: string | undefined;
  try {
    if (sources) credits = servingCredits(sources);
  } catch (error) {
    missing = error instanceof Error ? error.message : "Credits unavailable";
  }
  // A fragment may target an entry rendered only after the status request.
  useEffect(() => {
    if (!status.data) return;
    try {
      document.getElementById(decodeURIComponent(window.location.hash.slice(1)))?.scrollIntoView();
    } catch {
      /* An invalid URL fragment has no matching entry. */
    }
  }, [status.data]);
  return (
    <div className="app privacy-page">
      <header className="topbar">
        <Brand />
      </header>
      <main className="privacy">
        <h1>OCR credits</h1>
        <p>
          Optical character recognition (OCR) is machine-read text. These engines and models supplied our text in place
          of missing or poorly read Library of Congress text in the search index the site serves now. The “Our OCR” link
          on a page identifies its engine below.
        </p>
        <p>
          For American Stories dataset credits, see <a href="/datasets#ds-american-stories">dataset details</a>.
        </p>
        {status.isPending && <p role="status">Loading credits…</p>}
        {status.isError && <p role="alert">OCR credits could not be loaded. Please try again.</p>}
        {status.data && <p>Serving version: {status.data.published.index_version}.</p>}
        {status.data && !sources && (
          <p role="alert">This server does not provide the serving version’s OCR inventory.</p>
        )}
        {sources?.ja_latin && !sources.ja_latin.by_engine && (
          <p role="alert">This server does not provide engine versions for its Japanese-language pages.</p>
        )}
        {missing && <p role="alert">{missing}. The serving version names an engine without a complete credit entry.</p>}
        {sources && !missing && !credits.length && !sources.ja_latin && (
          <p>The serving version has no pages with our replacement OCR text.</p>
        )}
        {credits.map((c) => (
          <section key={c.key} id={c.key} className="status-section">
            <h2>{c.key}</h2>
            <p>
              {count(c.pages)} pages.{" "}
              {c.japanese
                ? "Japanese-language newspapers whose Library of Congress text was missing or poorly read."
                : "Other newspapers whose Library of Congress text misread many common words."}{" "}
              Language:{" "}
              {c.languages
                .map((l) =>
                  languageName(({ deu: "ger", ces: "cze", ell: "gre", zho: "chi" } as Record<string, string>)[l] ?? l),
                )
                .join(", ")}
              .
            </p>
            <Credit name={`${c.engine.family} — version ${c.version}`} entry={c.engine} required={required} />
            {c.models.map((m) => (
              <Credit
                key={m.name}
                name={`${m.name} — ${c.japanese ? `bundled with engine ${c.version}` : m.version ? `model version ${m.version}` : "model revision not recorded in the serving inventory"}`}
                entry={m}
                required={required}
              />
            ))}
          </section>
        ))}
      </main>
      <footer className="credits">
        <a href="/">Search</a> · <a href="/datasets">Dataset details</a> · <a href="/privacy">Privacy</a>
      </footer>
    </div>
  );
}
