import { useId, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import type { Meta, Mode } from "../api/types";
import { hasJapanese } from "../lib/japanese";
import { DEFAULTS, type ViewState } from "../state/url";
import { LanguageFilter, languageChoices } from "./LanguageFilter";

interface Props {
  view: ViewState;
  meta: Meta | undefined;
  onSearch: (patch: Partial<ViewState>) => void;
}

const MODES: { value: Mode; label: string }[] = [
  { value: "phrase", label: "Exact phrase" },
  { value: "all", label: "All words" },
  { value: "any", label: "Any word" },
  { value: "near", label: "Words near each other" },
];

/** How many search options differ from the defaults (shown on the toggle). */
export function activeOptions(view: Pick<ViewState, "mode" | "from" | "to" | "state" | "lang">): number {
  return [
    view.mode !== DEFAULTS.mode,
    view.from !== DEFAULTS.from,
    view.to !== DEFAULTS.to,
    view.state.length > 0,
    view.lang.length > 0,
  ].filter(Boolean).length;
}

/**
 * What a query in Japanese script searches (#139): only the Japanese pages we
 * read ourselves, or nothing yet on a version without them. Null otherwise.
 */
export function japaneseHint(q: string, meta: Pick<Meta, "ja"> | undefined): string | null {
  // Before /v1/meta loads, it isn't known yet whether Japanese search is there.
  if (!hasJapanese(q) || !meta) return null;
  const ja = meta.ja;
  if (!ja) return "Searching Japanese text isn't available yet. It arrives with the next update.";
  return `Japanese searches cover only the ${ja.pages.toLocaleString("en-US")} Japanese-language pages we read ourselves. The Library of Congress has no searchable text for them.`;
}

/** Identity of the search in the URL; remount the form when it changes. */
export function searchKey(view: ViewState): string {
  return [
    view.q,
    view.mode,
    view.near,
    view.from,
    view.to,
    view.state.join(),
    view.lang.join(),
    view.lccn.join(),
  ].join("|");
}

export function SearchBar({ view, meta, onSearch }: Props) {
  const id = useId();
  // The parent remounts this form (via `key`) when the search in the URL
  // changes, so back/forward and example cards replace the draft.
  const [draft, setDraft] = useState(view);
  const [states, setStates] = useState(view.state.join(", "));
  // Codes from the URL stay listed after they're unchecked, so they don't vanish.
  const languages = languageChoices(meta?.languages, [...new Set([...view.lang, ...draft.lang])]);
  // On phones the options (match mode, dates, states, languages) sit behind a toggle so
  // the header is just the search box. They start open when a search already
  // uses one, so an active filter is never hidden. Wider screens always show
  // them (CSS hides the toggle).
  const [open, setOpen] = useState(() => activeOptions(view) > 0);
  const active = activeOptions({ ...draft, state: states.split(/[\s,]+/).filter(Boolean) });

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!draft.q.trim()) return;
    onSearch({
      q: draft.q.trim(),
      mode: draft.mode,
      near: draft.near,
      from: draft.from,
      to: draft.to,
      state: states
        .split(/[\s,]+/)
        .map((s) => s.toUpperCase())
        .filter((s) => /^[A-Z]{2}$/.test(s)),
      lang: draft.lang,
    });
  };

  const lo = meta?.bounds.from;
  const hi = meta?.bounds.to;
  const jaHint = japaneseHint(draft.q, meta);
  // An input method (IME) uses Enter to confirm a word: that Enter mustn't
  // submit the search. Safari ends the composition before the keydown, so
  // keyCode 229 ("being composed") is checked too.
  const composing = useRef(false);
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" && (composing.current || e.nativeEvent.isComposing || e.keyCode === 229)) {
      e.preventDefault();
    }
  };
  return (
    <form
      className={open ? "search search--open" : "search"}
      role="search"
      onSubmit={submit}
      aria-label="Search newspapers"
    >
      <label className="visually-hidden" htmlFor={`${id}-q`}>
        Word or phrase
      </label>
      <input
        id={`${id}-q`}
        className="search__q"
        type="search"
        placeholder='Try "cross of gold"'
        value={draft.q}
        maxLength={meta?.limits.max_query_chars ?? 256}
        onChange={(e) => setDraft({ ...draft, q: e.target.value })}
        onKeyDown={onKeyDown}
        onCompositionStart={() => (composing.current = true)}
        onCompositionEnd={() => (composing.current = false)}
        aria-describedby={jaHint ? `${id}-ja` : undefined}
        autoComplete="off"
        spellCheck={false}
      />
      <button type="submit" className="button button--primary search__go">
        Search
      </button>
      {jaHint && (
        <p id={`${id}-ja`} className="search__hint" role="note">
          {jaHint}
        </p>
      )}
      <button
        type="button"
        className="button search__toggle"
        aria-expanded={open}
        aria-controls={`${id}-options`}
        onClick={() => setOpen(!open)}
      >
        Options{active > 0 ? ` (${active})` : ""}
      </button>
      <div className="search__options" id={`${id}-options`}>
        <label className="visually-hidden" htmlFor={`${id}-mode`}>
          Match
        </label>
        <select
          id={`${id}-mode`}
          value={draft.mode}
          onChange={(e) => setDraft({ ...draft, mode: e.target.value as Mode })}
        >
          {MODES.map((m) => (
            <option key={m.value} value={m.value}>
              {m.label}
            </option>
          ))}
        </select>
        {draft.mode === "near" && (
          <label className="search__near">
            within
            <input
              type="number"
              min={1}
              max={meta?.limits.max_slop ?? 20}
              value={draft.near}
              onChange={(e) => setDraft({ ...draft, near: Number(e.target.value) || 1 })}
            />
            words
          </label>
        )}
        <label className="search__date">
          <span>From</span>
          <input
            type="date"
            min={lo}
            max={hi}
            value={draft.from}
            onChange={(e) => setDraft({ ...draft, from: e.target.value })}
          />
        </label>
        <label className="search__date">
          <span>To</span>
          <input
            type="date"
            min={lo}
            max={hi}
            value={draft.to}
            onChange={(e) => setDraft({ ...draft, to: e.target.value })}
          />
        </label>
        <label className="search__state">
          <span>States</span>
          <input
            placeholder="e.g. GA, SC"
            value={states}
            onChange={(e) => setStates(e.target.value)}
            pattern="^\s*([A-Za-z]{2}([\s,]+|$))*$"
            title="Two-letter state codes, separated by commas"
          />
        </label>
        {languages.length > 0 && (
          <LanguageFilter
            id={id}
            choices={languages}
            value={draft.lang}
            onChange={(lang) => setDraft({ ...draft, lang })}
          />
        )}
      </div>
    </form>
  );
}
