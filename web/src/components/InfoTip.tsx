import { useEffect, useId, useRef, useState, type ReactNode } from "react";

/**
 * A small "i" button that shows an explanation on a tap (not hover, so it
 * works on phones). Escape or a tap elsewhere closes it. `up` opens it above
 * the button, for one near the bottom of the map.
 */
export function InfoTip({ label, up = false, children }: { label: string; up?: boolean; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const id = useId();
  const wrap = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: Event) => {
      if (e instanceof KeyboardEvent ? e.key === "Escape" : !wrap.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("keydown", close);
    document.addEventListener("pointerdown", close);
    return () => {
      document.removeEventListener("keydown", close);
      document.removeEventListener("pointerdown", close);
    };
  }, [open]);
  return (
    <div className="infotip" ref={wrap}>
      <button
        type="button"
        className="infotip__button"
        aria-expanded={open}
        aria-controls={id}
        aria-label={label}
        onClick={() => setOpen((o) => !o)}
      >
        <span aria-hidden="true">i</span>
      </button>
      <div id={id} className={up ? "infotip__body infotip__body--up" : "infotip__body"} hidden={!open}>
        {children}
      </div>
    </div>
  );
}
