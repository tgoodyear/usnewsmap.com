// The search form's language filter (07 §7.9): a button that opens a
// checklist of the catalog's languages. A choice applies with the rest of
// the form when the visitor presses Search.

import { useEffect, useRef, useState } from "react";
import type { MetaLanguage } from "../api/types";
import { languageName } from "../lib/languages";

export interface LanguageChoice {
  code: string;
  name: string;
  /** Newspapers that list it; null for a code the catalog doesn't have. */
  titles: number | null;
}

/**
 * The checklist: the catalog's languages in the API's order (most pages
 * first), then any chosen code the catalog doesn't list, so it can be cleared.
 */
export function languageChoices(list: readonly MetaLanguage[] | undefined, chosen: readonly string[]): LanguageChoice[] {
  const out: LanguageChoice[] = (list ?? []).map((l) => ({
    code: l.code,
    name: languageName(l.code, l.name),
    titles: l.titles,
  }));
  const known = new Set(out.map((c) => c.code));
  for (const code of chosen) if (!known.has(code)) out.push({ code, name: languageName(code), titles: null });
  return out;
}

/** "Languages: any", "Languages: German", "Languages: German, Spanish", "Languages: 3 chosen". */
export function languageSummary(chosen: readonly string[], choices: readonly LanguageChoice[]): string {
  const name = (code: string) => choices.find((c) => c.code === code)?.name ?? languageName(code);
  if (chosen.length === 0) return "Languages: any";
  if (chosen.length <= 2) return `Languages: ${chosen.map(name).join(", ")}`;
  return `Languages: ${chosen.length} chosen`;
}

export function choiceLabel(c: LanguageChoice): string {
  if (c.titles === null || c.titles === 0) return `${c.name} (no newspapers)`;
  return `${c.name} (${c.titles.toLocaleString("en-US")} ${c.titles === 1 ? "newspaper" : "newspapers"})`;
}

interface Props {
  id: string;
  choices: LanguageChoice[];
  /** Chosen codes, sorted. */
  value: string[];
  onChange: (codes: string[]) => void;
}

export function LanguageFilter({ id, choices, value, onChange }: Props) {
  const [open, setOpen] = useState(false);
  const wrap = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => {
      if (!wrap.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", away);
    return () => document.removeEventListener("pointerdown", away);
  }, [open]);
  const toggle = (code: string, on: boolean) =>
    onChange(on ? [...new Set([...value, code])].sort() : value.filter((c) => c !== code));
  const listId = `${id}-langs`;
  return (
    <div
      className="langs"
      ref={wrap}
      onKeyDown={(e) => {
        if (e.key === "Escape" && open) {
          e.preventDefault();
          setOpen(false);
          button.current?.focus();
        }
      }}
    >
      <button
        ref={button}
        type="button"
        className="button langs__button"
        aria-expanded={open}
        aria-controls={listId}
        onClick={() => setOpen((o) => !o)}
      >
        {languageSummary(value, choices)}
      </button>
      <fieldset id={listId} className="langs__list" hidden={!open}>
        <legend>Newspaper languages</legend>
        <p className="langs__hint">Pages from newspapers in any of these. Leave all unchecked for every language.</p>
        <ul>
          {choices.map((c) => (
            <li key={c.code}>
              <label>
                <input
                  type="checkbox"
                  value={c.code}
                  checked={value.includes(c.code)}
                  onChange={(e) => toggle(c.code, e.target.checked)}
                />{" "}
                {choiceLabel(c)}
              </label>
            </li>
          ))}
        </ul>
        <button type="button" className="link-button" onClick={() => onChange([])}>
          Clear languages
        </button>
      </fieldset>
    </div>
  );
}
