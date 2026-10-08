import { useEffect, useId, useRef, useState, type ReactNode } from "react";

/** Widest the note gets, and its margin from the window's edges. */
const WIDTH = 320;
const EDGE = 8;

/**
 * A short note on text already on the page, shown while the pointer is over
 * it or it has keyboard focus (the WAI-ARIA tooltip pattern). The text takes
 * focus and the note describes it (`aria-describedby`), so a screen reader
 * reads the note with it. It stays while either the pointer or the focus
 * is on the text, and goes when neither is, or on Escape (until the pointer
 * or the focus comes back). On phones a tap focuses the text and shows the
 * note. Fixed to the window, so a toolbar that cuts its text short doesn't
 * cut the note.
 */
export function Tooltip({ note, children }: { note: string; children: ReactNode }) {
  const id = useId();
  const ref = useRef<HTMLSpanElement>(null);
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const [at, setAt] = useState<{ left: number; top: number } | null>(null);
  // Under the text, kept inside the window.
  const place = () => {
    const r = ref.current?.getBoundingClientRect();
    if (!r) return null;
    return { left: Math.max(EDGE, Math.min(r.left, window.innerWidth - EDGE - WIDTH)), top: r.bottom + 6 };
  };
  // The pointer or the focus arrives: show it, even after an Escape.
  const arrive = () => {
    setDismissed(false);
    setAt(place());
  };
  const open = (hovered || focused) && !dismissed && at !== null;
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setDismissed(true);
    };
    // The page or a panel scrolled, or the window changed size: follow the text.
    const onMove = () => {
      const p = place();
      if (p) setAt((a) => (a && (a.top !== p.top || a.left !== p.left) ? p : a));
    };
    document.addEventListener("keydown", onKey);
    window.addEventListener("scroll", onMove, true);
    window.addEventListener("resize", onMove);
    return () => {
      document.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", onMove, true);
      window.removeEventListener("resize", onMove);
    };
  }, [open]);
  return (
    <span
      ref={ref}
      className="tooltip"
      tabIndex={0}
      aria-describedby={id}
      onMouseEnter={() => {
        setHovered(true);
        arrive();
      }}
      onMouseLeave={() => setHovered(false)}
      onFocus={() => {
        setFocused(true);
        arrive();
      }}
      onBlur={() => setFocused(false)}
    >
      {children}
      <span
        id={id}
        role="tooltip"
        className="tooltip__body"
        hidden={!open}
        style={open && at ? { left: at.left, top: at.top, maxWidth: `min(${WIDTH}px, calc(100vw - ${2 * EDGE}px))` } : undefined}
      >
        {note}
      </span>
    </span>
  );
}
