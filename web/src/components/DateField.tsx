import { useRef, type Ref } from "react";
import { parseDateEntry, showDate, type DateBounds, type Edge } from "../lib/dateEntry";

interface Props {
  id: string;
  label: string;
  edge: Edge;
  /** What the box shows, as typed. */
  text: string;
  onText: (text: string) => void;
  /** What's wrong with the entry, shown under the form (null when nothing is). */
  error: string | null;
  /** The id of the element showing `error`. */
  errorId: string;
  onError: (error: string | null) => void;
  bounds: DateBounds | undefined;
  inputRef?: Ref<HTMLInputElement>;
}

/** Browsers that can open a date input's calendar from a script. */
function canShowPicker(): boolean {
  return typeof HTMLInputElement !== "undefined" && "showPicker" in HTMLInputElement.prototype;
}

/**
 * A From or To date. A text box rather than a date input, which reports
 * nothing until all of mm, dd and yyyy are filled in, so a year or a month
 * on its own can be read (see lib/dateEntry). The calendar button opens a
 * hidden date input's own picker.
 */
export function DateField({ id, label, edge, text, onText, error, errorId, onError, bounds, inputRef }: Props) {
  const native = useRef<HTMLInputElement>(null);
  const parsed = parseDateEntry(text, edge, bounds);
  // Once the box is left, show the date the entry stands for (1827 is 01/01/1827 in From).
  // Not before /v1/meta gives the index's first and last days: a year that
  // overlaps them is cut to them, which a filled-in 01/01 would no longer be.
  const commit = () => {
    if (parsed.ok) {
      if (bounds) onText(showDate(parsed.iso));
      onError(null);
    } else {
      onError(parsed.error);
    }
  };
  const openPicker = () => {
    try {
      native.current?.showPicker();
    } catch {
      // Not allowed here (e.g. inside a cross-origin frame): typing still works.
    }
  };
  return (
    <div className="search__date">
      <label htmlFor={`${id}-${edge}`}>{label}</label>
      <span className="date-field">
        <input
          id={`${id}-${edge}`}
          ref={inputRef}
          className="date-field__text"
          type="text"
          placeholder="mm/dd/yyyy"
          value={text}
          onChange={(e) => {
            onText(e.target.value);
            if (error) onError(null);
          }}
          onBlur={commit}
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? `${id}-date-hint ${errorId}` : `${id}-date-hint`}
          autoComplete="off"
          spellCheck={false}
        />
        {canShowPicker() && (
          <button
            type="button"
            className="date-field__pick"
            aria-label={`Choose the ${label} date on a calendar`}
            onClick={openPicker}
          >
            <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true" focusable="false">
              <rect x="1.5" y="2.5" width="13" height="12" rx="1.5" fill="none" stroke="currentColor" />
              <path d="M1.5 6h13M5 1v3M11 1v3" fill="none" stroke="currentColor" />
            </svg>
          </button>
        )}
        <input
          ref={native}
          className="date-field__native"
          type="date"
          tabIndex={-1}
          aria-hidden="true"
          min={bounds?.from}
          max={bounds?.to}
          value={parsed.ok ? parsed.iso : ""}
          onChange={(e) => {
            onText(showDate(e.target.value));
            onError(null);
          }}
        />
      </span>
    </div>
  );
}
