/**
 * The results area while a new search runs and nothing of it is shown yet
 * (#210): what is being searched, a spinner, and the API's queue note when it
 * is still computing the search (a `202`).
 */
export function Searching({ q, note }: { q: string; note: string | null }) {
  return (
    <div className="searching" role="status">
      <span className="spinner spinner--large" aria-hidden="true" />
      <p className="searching__title">Searching for {quoted(q)}…</p>
      {note && <p className="searching__note">{note}</p>}
    </div>
  );
}

/**
 * The query in curly quotes. A query with straight quotes of its own (a
 * phrase, `"lend lease"`, or `"new york" fire`) shows those as curly quotes
 * instead, rather than inside a second pair.
 */
export function quoted(q: string): string {
  const text = q.trim();
  if (!text.includes('"')) return `“${text}”`;
  let open = true;
  return text.replace(/"/g, () => {
    const mark = open ? "“" : "”";
    open = !open;
    return mark;
  });
}
