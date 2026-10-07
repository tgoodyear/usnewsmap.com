import { useEffect, useId, useRef, useState, type ReactNode } from "react";

/** Widest the note gets, and its margin from the window's edges. */
const WIDTH = 320;
const EDGE = 8;

/**
 * A short note on text already on the page, shown while the pointer is over
 * it or it has keyboard focus (the WAI-ARIA tooltip pattern). The text takes
 * focus and the note describes it (`aria-describedby`), so a screen reader
 * reads the note with it; Escape hides it. On phones a tap focuses the text
 * and shows the note. Fixed to the window, so a toolbar that cuts its text
 * short doesn't cut the note.
 */
export function Tooltip({ note, children }: { note: string; children: ReactNode }) {
  const id = useId();
  const ref = useRef<HTMLSpanElement>(null);
  const [at, setAt] = useState<{ left: number; top: number } | null>(null);
  // Under the text, kept inside the window.
  const show = () => {
    const r = ref.current?.getBoundingClientRect();
    if (!r) return;
    const left = Math.max(EDGE, Math.min(r.left, window.innerWidth - EDGE - WIDTH));
    setAt({ left, top: r.bottom + 6 });
  };
  const hide = () => setAt(null);
  const open = at !== null;
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setAt(null);
    };
    // The page or a panel scrolled: follow the text.
    const onMove = () => {
      const r = ref.current?.getBoundingClientRect();
      if (r) setAt((a) => (a && a.top !== r.bottom + 6 ? { ...a, top: r.bottom + 6 } : a));
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
      onMouseEnter={show}
      onMouseLeave={hide}
      onFocus={show}
      onBlur={hide}
    >
      {children}
      <span
        id={id}
        role="tooltip"
        className="tooltip__body"
        hidden={!at}
        style={at ? { left: at.left, top: at.top, maxWidth: `min(${WIDTH}px, calc(100vw - ${2 * EDGE}px))` } : undefined}
      >
        {note}
      </span>
    </span>
  );
}
