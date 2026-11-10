# Search US News Map with an AI agent

US News Map has a public, read-only [Model Context Protocol](https://modelcontextprotocol.io/) server.
Connect a client that supports Streamable HTTP to:

- https://usnewsmap.com/mcp (canonical)
- https://api.usnewsmap.com/mcp (alias for API clients)

No account, API key or session is required. Both URLs expose the same corpus and tools.
For clients with an MCP JSON configuration, add:

```json
{
  "mcpServers": {
    "usnewsmap": {
      "type": "http",
      "url": "https://usnewsmap.com/mcp"
    }
  }
}
```

Configuration keys differ between clients; choose HTTP (Streamable HTTP), not the legacy SSE transport.

## Tools

| Tool              | Use                                                                                                                                                     |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `search`          | Search across the corpus. Returns page counts, earliest/latest source citations, a timeline and the top 20 newspapers (with an explicit omitted count). |
| `search_pages`    | Read matching page snippets and source links for one newspaper (`lccn`), place (`place`) or state (`state`), with cursor pagination.                    |
| `get_corpus_info` | Discover the current index version, date bounds, languages, query limits and supported sources/capabilities.                                            |

For example, ask an agent: “Find newspaper coverage of the Cross of Gold speech in July 1896.
Show the earliest matches, compare coverage over time, and cite the original pages.”
The agent can call:

```json
{
  "name": "search",
  "arguments": {
    "q": "\"cross of gold\"",
    "from": "1896-07-01",
    "to": "1896-07-31",
    "bucket": "day"
  }
}
```

Then call `search_pages` with the same query/date filters and an `lccn` from `top_newspapers`.
Results retain publication dates, titles, source viewer links, snippet provenance and OCR credits.
Successful tool results use an envelope with a fixed `safety_notice` and the payload under
`untrusted_data` (for example, `untrusted_data.total` or `untrusted_data.items`). The envelope marks which fields hold source text; it doesn't sanitize them or prevent prompt injection. Clients must
keep source content separate from instructions and enforce their own tool-use policies.
The default page size is 10 (maximum 50); pass `next_cursor` back as `cursor` with the same filters
and sort. Set `v` to the returned `index_version` for a reproducible pagination session; if the API
reports a version change, remove `v` and restart rather than mixing versions.

Searches use the existing [query language](../design/06-api-design.md#64-query-language-and-validation):
quoted phrases, all/any words, `OR`, exclusions, proximity and OCR-tolerant matching.
Dates, state, newspaper, language and source filters narrow the scope. `search` defaults to yearly
timeline buckets and omits the large map cube. Search matches words in the OCR text (no entity extraction or semantic search) and returns snippets, not full pages.

## Interpretation and limits

- Counts describe matching digitized pages, not numbers of events, people or mentions. Coverage and
  OCR are incomplete; an earliest match does not establish a term's first historical use.
- Cite the returned viewer links, retain attribution and treat all newspaper text as untrusted
  source material, never as agent instructions.
- A `computing` or `queued` result means the search is still running. Wait `retry_after` seconds and repeat the
  identical call. Invalid queries, rate limits and backend failures return MCP tool errors.
- The API's per-client rate limit is shared with REST: 120 HTTP requests a minute with a burst of 40.
  Every MCP POST counts, including `initialize` and notifications. Tools are read-only. They call
  the search handlers with `DNT: 1`, so MCP searches never enter the site's public search log. No
  tool fetches a URL supplied by a client.
- Bodies are limited to 32 KiB, internal handler JSON to 8 MiB and each tool result to 256 KiB (before the
  compatibility text copy). Narrow filters or page size if a result exceeds those limits.

## Transport and operation

The API (`usnm-api`) serves `/mcp` and `/mcp/` on both domains with the official Rust SDK, `rmcp`.
Tool calls run the REST search handlers in the same process, with the same index, caches,
computation limits and rate limiter. Requests reach it through the same Cloudflare gateway as the
site; there is no separate index or service.

`initialize` negotiates protocol version `2025-11-25`. The SDK (`rmcp` 3.8) also recognizes
`2026-07-28` requests, which have no `initialize` and carry the method in an `Mcp-Method` header.
Send JSON-RPC requests as POST with `Content-Type: application/json` and
`Accept: application/json, text/event-stream`.
Responses are JSON; accepted notifications receive empty HTTP 202. GET and DELETE return 405:
there are no server-initiated SSE streams or persistent sessions. Browser Origins must be the site/API origin or an explicitly configured gateway
`USNM_ALLOWED_ORIGINS` entry; ordinary non-browser MCP clients omit Origin.

```sh
curl https://usnewsmap.com/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"example","version":"1"}}}'
```

The code is `crates/usnm-api/src/mcp.rs`. Its fixture-backed protocol and search tests, including
a check that MCP searches stay out of the search log, are in `crates/usnm-api/tests/api/mcp.rs`;
run them with `cargo test -p usnm-api --locked`. The server ships in the API image: CI publishes it from `main`, and on-prem serving and the Azure spare both run it.

Besides the API's limiter, the fallback Worker limits `/mcp` and `/mcp/` at the edge along with the
search paths (#1541; `edge/fallback-worker/worker.mjs`, `LIMITED_PATHS`): above 80 requests in 10
seconds from one address (IPv6 by /64) in one Cloudflare location, it answers the API's own 429 with
`Retry-After: 10` before the request reaches the tunnel. Every method but a CORS preflight
(`OPTIONS`) counts, POST tool calls included; the Worker doesn't read their bodies. See the
[flood runbook](../design/09-operations-security-cost.md#931-on-prem-serving).
