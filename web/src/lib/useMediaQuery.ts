import { useCallback, useSyncExternalStore } from "react";

/** Whether a CSS media query matches, kept current as the viewport changes. */
export function useMediaQuery(query: string): boolean {
  // Stable per query, so re-renders (every playback frame) don't re-subscribe the listener.
  const subscribe = useCallback(
    (onChange: () => void) => {
      const list = window.matchMedia(query);
      list.addEventListener("change", onChange);
      return () => list.removeEventListener("change", onChange);
    },
    [query],
  );
  const snapshot = useCallback(() => window.matchMedia(query).matches, [query]);
  return useSyncExternalStore(subscribe, snapshot, () => false);
}
