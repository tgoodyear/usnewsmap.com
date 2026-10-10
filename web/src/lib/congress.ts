import type { CongressMeta, PlaceFeature } from "../api/types";

/**
 * The map feature for the one place every Congress page sits at for now
 * (#229): Washington, DC, from `/v1/meta`, kept apart from DC's newspapers.
 */
export function congressFeature(place: CongressMeta["place"]): PlaceFeature {
  return {
    type: "Feature",
    id: place.id,
    geometry: { type: "Point", coordinates: [place.lon, place.lat] },
    properties: {
      name: place.name,
      state: "DC",
      precision: "city",
      titles: 0,
      languages: ["eng"],
    },
  };
}
