// Shared by the entry-bundle cache warmer and the lazy map component.
import manifest from "../../basemap.json";

export const STYLE_URL = import.meta.env.VITE_BASEMAP_STYLE ?? `/basemap/${manifest.version}/style.json`;
export const US_CENTER: [number, number] = [-96, 38.5];
export const DEFAULT_ZOOM = 3.3;
