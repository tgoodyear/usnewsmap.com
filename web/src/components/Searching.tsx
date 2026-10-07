/**
 * The results area while a new search runs and nothing of it is shown yet
 * (#210): what is being searched, a spinner, and the API's queue note when it
 * is still computing the search (a `202`).
 */
export function Searching({ q, note }: { q: string; note: string | null }) {
  return (
    <div className="searching" role="status">
      <span className="spinner spinner--large" aria-hidden="true" />
      <p className="searching__title">Searching for “{q}”…</p>
      {note && <p className="searching__note">{note}</p>}
    </div>
  );
}
