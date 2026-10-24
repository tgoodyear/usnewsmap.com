import { useEffect } from "react";
import { TITLES } from "../route";
import { Brand } from "../components/Brand";

const ext = { target: "_blank", rel: "noopener noreferrer" } as const;

/**
 * `/privacy`: what the site collects. Every statement here is checked
 * against the code and the deployment (09 §9.4.2); change them together.
 *
 * DRAFT for on-prem serving behind Cloudflare (#955, ADR-0027). It ships with
 * the Phase 3 cutover and not before. The comments marked "Before Phase 3"
 * name what has to be settled first.
 */
export default function PrivacyPage() {
  useEffect(() => {
    document.title = TITLES.privacy;
  }, []);
  return (
    <div className="app privacy-page">
      <header className="topbar">
        <Brand />
      </header>
      <main className="privacy">
        <h1>Privacy</h1>
        {/* Before Phase 3: set this to the cutover date. */}
        <p className="privacy-updated">Last updated 15 October 2026.</p>
        <p>
          US News Map has no accounts, sets no cookies and saves nothing in your browser's storage. It shows no ads and
          runs no analytics or tracking scripts from other companies. Your browser's ordinary cache keeps copies of the
          site's files, as it does for any website.
        </p>

        <h2>Page views</h2>
        <p>When you open a page on this site, your browser sends the site's server a short page-view record with:</p>
        <ul>
          <li>
            the page's name and title (search, dataset details, privacy, OCR credits or page not found), never your
            search;
          </li>
          <li>
            the site that linked you here, cut down to its domain (for example https://www.google.com), or "direct" if
            there was none;
          </li>
          <li>any utm_source, utm_medium and utm_campaign tags in the link you followed.</li>
        </ul>
        <p>
          The server adds the page's fixed address (such as https://usnewsmap.com/datasets, with no search in it) and
          your browser family (such as Firefox) and device type (desktop, mobile or tablet), worked out from your
          browser's user agent. The user agent itself is not kept. The server then passes the record to Microsoft Azure
          Application Insights with your IP address. Azure uses the address to look up an approximate location (city,
          region and country), then stores 0.0.0.0 in its place. There is no user ID, session ID or other identifier
          that links page views to each other or to you.
        </p>
        <p>
          If your browser sends Do Not Track or Global Privacy Control, the site sends no page views, and the server
          discards any that arrive with either signal.
        </p>

        <h2>Searches</h2>
        {/* Before Phase 3: ADR-0027 settled decision 1 accepts that Cloudflare may keep search words with IP
            addresses. If Phase 2 shows its sample keeps no query string, drop the second sentence. */}
        <p>
          The site's own server never stores the words you search for with your IP address or anything else about you.
          Cloudflare, which carries every request, may keep them with your IP address for up to 31 days, as described
          below. The server keeps a record of each request it answers: the kind of request (such as search results or
          map places), its status and how long it took. These records leave out the page address, your search and your
          IP address. So that a repeated search is fast, the server caches search results for up to 14 days. A cached
          result includes the search words but nothing about who searched.
        </p>
        <p>
          The site keeps the words of every search indefinitely, to understand what people look for. Each search is
          stored with only the filters you chose (such as dates, states and match type), the number of pages found and
          the day it happened. It is never stored with your IP address, browser details, location or any identifier. If
          your browser sends Do Not Track or Global Privacy Control, your searches are not recorded. Only the site's
          maintainer can read this record. If a list of searches is ever published, it will include only searches made
          at least five times.
        </p>
        <p>
          To prioritize pages for improved OCR, the server may also keep the IDs of eligible newspaper pages shown in
          search results. It stores only page IDs grouped by day, not your search, filters, IP address or identity, and
          does not collect them when Do Not Track or Global Privacy Control is enabled. These IDs are used by the OCR
          system and the daily list is deleted after processing.
        </p>
        <p>
          To stop any one client from overloading the service, the server counts requests per IP address. It keeps a
          salted hash of the address in memory for this and never writes it anywhere.
        </p>

        <h2>Cloudflare</h2>
        <p>
          Every request to this site passes through Cloudflare, which delivers the site and blocks some attacks before
          they reach its server. Cloudflare decrypts each request to pass it on, so it sees what the site's server sees:
          your IP address, the full web address you asked for (which includes the words of a search) and the headers
          your browser sends, such as its user agent. It sees all of this even when your browser sends Do Not Track or
          Global Privacy Control.
        </p>
        {/* Before Phase 3: the 31-day sample is what Security Analytics shows on the Free plan. Phase 2 measures
            whether it keeps the query string; if it doesn't, drop "which may include the words of a search". "Sets
            no cookies" holds while Bot Fight Mode is off and the flood rule's action stays Block (ADR-0027 settled
            decision 9). */}
        <p>
          Cloudflare shows the site's maintainer a sample of recent requests going back 31 days. Each one shows the IP
          address it came from and the web address it asked for, which may include the words of a search. For this
          sample and for the content that passes through its network, Cloudflare is a processor: it handles them on the
          site's behalf and follows the site's instructions. For other information about your traffic, Cloudflare is a
          controller, which means it decides how that information is used. The{" "}
          <a href="https://www.cloudflare.com/privacypolicy/" {...ext}>
            Cloudflare privacy policy
          </a>{" "}
          sets out both. Cloudflare sets no cookies on this site.
        </p>
        <p>
          To make pages load faster, Cloudflare keeps copies of the site's files and of data such as its lists of places
          and newspapers. It never keeps copies of search results, so every search reaches the site's server.
        </p>

        <h2>Storage and retention</h2>
        {/* Before Phase 3: the server's location, and journald on the serving host kept to 30 days so that
            "other logs after 30 days" still holds. */}
        <p>
          The site's server runs on hardware its maintainer operates. Microsoft Azure stores the page views, request
          records, error reports and the search record in its East US 2 region (Virginia, United States). Page views,
          request records and the server's error reports are deleted after 90 days, and its other logs after 30 days.
          The search words described above are kept indefinitely.
        </p>

        <h2>Legal basis</h2>
        <p>
          The site uses this data for aggregate statistics, such as visits per day and which sites send visitors, and to
          keep the service working. That is a legitimate interest, and the site's own records keep no IP address, cookie
          or identifier. The same legitimate interest covers the site's use of Cloudflare to deliver its pages and keep
          the service working. The site doesn't ask for consent because it stores nothing on your device.
        </p>

        <h2>Other sites</h2>
        <p>
          The map background comes from OpenFreeMap. Your browser requests map tiles and fonts from OpenFreeMap's
          servers, under the{" "}
          <a href="https://openfreemap.org/privacy/" {...ext}>
            OpenFreeMap privacy policy
          </a>
          . Newspaper pages open on the Library of Congress website, under the{" "}
          <a href="https://www.loc.gov/legal/security-copyright-and-privacy/privacy-policy/" {...ext}>
            Library of Congress privacy policy
          </a>
          .
        </p>

        <h2>Contact</h2>
        <p>
          US News Map is maintained by Trevor Goodyear. For questions about this page, use the contact form at{" "}
          <a href="https://goodyeartechnical.com/contact/" {...ext}>
            goodyeartechnical.com/contact
          </a>
          .
        </p>
      </main>
      <footer className="credits">
        <a href="/">Search</a> · <a href="/datasets">Dataset details</a> · <a href="/credits">OCR credits</a>
      </footer>
    </div>
  );
}
