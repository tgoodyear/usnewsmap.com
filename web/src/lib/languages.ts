// Title languages for the relative-rate view (doc 11, 11.14). An English
// search term rarely matches pages printed in another language, so a place
// with such papers can read low; the view says which languages its papers
// are in. Codes and names follow the catalog's table (crates/usnm-core/src/
// names.rs once #66 merges; crates/usnm-ingest/src/titles.rs before it).

const NAMES: Record<string, string> = {
  eng: "English", ger: "German", spa: "Spanish", fre: "French", ita: "Italian", pol: "Polish", cze: "Czech",
  slo: "Slovak", slv: "Slovenian", lit: "Lithuanian", swe: "Swedish", nor: "Norwegian", dan: "Danish",
  dut: "Dutch", fin: "Finnish", hun: "Hungarian", yid: "Yiddish", heb: "Hebrew", chi: "Chinese",
  jpn: "Japanese", por: "Portuguese", rus: "Russian", ukr: "Ukrainian", gre: "Greek", rum: "Romanian",
  hrv: "Croatian", srp: "Serbian", ara: "Arabic", arm: "Armenian", wel: "Welsh", ice: "Icelandic",
  lat: "Latin", haw: "Hawaiian", chr: "Cherokee", cho: "Choctaw", dak: "Dakota", oji: "Ojibwa",
  baq: "Basque", gle: "Irish", tgl: "Tagalog", kor: "Korean", est: "Estonian", lav: "Latvian",
  bul: "Bulgarian", alb: "Albanian", mus: "Creek",
};

export function languageName(code: string): string {
  // Unlisted languages arrive as the catalog's lowercase name, which may
  // have several words ("pennsylvania german"): title-case each word.
  return NAMES[code] ?? code.replace(/(^|[\s-])(\p{L})/gu, (_, sep: string, ch: string) => sep + ch.toUpperCase());
}

/**
 * "Papers in German", "Papers in Serbian and English", or null when every
 * listed language is English (or none is listed). Other languages first,
 * English last.
 */
export function languageLabel(codes: readonly string[] | undefined): string | null {
  const list = [...new Set(codes ?? [])];
  if (!list.some((c) => c !== "eng")) return null;
  const names = list
    .filter((c) => c !== "eng")
    .map(languageName)
    .sort((a, b) => a.localeCompare(b));
  if (list.includes("eng")) names.push("English");
  const joined = names.length === 1 ? names[0]! : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
  return `Papers in ${joined}`;
}
