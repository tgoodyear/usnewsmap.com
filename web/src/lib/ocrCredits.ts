/** The fragment is the exact short engine key returned on a hit. */
export function creditHref(engine: string | null): string {
  if (!engine) return "/credits";
  return `/credits#${encodeURIComponent(engine)}`;
}
