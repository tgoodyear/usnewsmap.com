import type { WordVerdict } from "../api/types";

/** The reader's own marks, kept in this browser only. Nothing here is sent anywhere. */
const KEY = "usnm:word-marks:v1";

type Store = Record<string, Record<string, WordVerdict>>;

function read(): Store {
  try {
    const raw = window.localStorage.getItem(KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? (parsed as Store) : {};
  } catch {
    return {};
  }
}

export function loadMarks(docId: string): Record<string, WordVerdict> {
  const marks = read()[docId];
  return marks && typeof marks === "object" ? { ...marks } : {};
}

/** Remembers a verdict. Storage that is full or blocked loses the mark quietly. */
export function saveMark(docId: string, wordId: string, verdict: WordVerdict): void {
  try {
    const store = read();
    store[docId] = { ...store[docId], [wordId]: verdict };
    window.localStorage.setItem(KEY, JSON.stringify(store));
  } catch {
    // Private mode or quota: the page still works without the memory.
  }
}
