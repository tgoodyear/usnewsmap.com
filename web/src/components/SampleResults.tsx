import { useInfiniteQuery } from "@tanstack/react-query";
import type { SearchParams } from "../api/client";
import type { AggregateResponse } from "../api/types";
import { Hit } from "./PlacePanel";
import { placeHitsQuery } from "./placeHits";

export function topPlace(places: AggregateResponse["places"]): string | null {
  let top: string | null = null;
  let max = 0;
  places.id.forEach((id, i) => {
    const hits = places.hits[i] ?? 0;
    if (hits > max || (hits === max && hits > 0 && (top === null || id < top))) {
      top = id;
      max = hits;
    }
  });
  return top;
}

export function SampleResults({
  params,
  version,
  placeId,
  placeName,
  synthetic,
}: {
  params: SearchParams;
  version: string;
  placeId: string;
  placeName: string;
  synthetic: boolean;
}) {
  const query = useInfiniteQuery(placeHitsQuery(params, version, placeId, "oldest"));
  const first = query.data?.pages[0];
  return (
    <section aria-label="Sample results" className="visually-hidden">
      <h2>Sample results</h2>
      <p>{placeName} has the most hits. Showing up to 10 earliest results from this city.</p>
      {query.isPending && <p role="status">Loading sample results…</p>}
      {query.error && <p role="alert">Could not load sample results. {query.error.message}</p>}
      {first && (
        <ol className="hits">
          {first.items.slice(0, 10).map((hit) => (
            <Hit key={hit.doc_id} h={hit} synthetic={synthetic} />
          ))}
        </ol>
      )}
    </section>
  );
}
