// Snippets arrive HTML-escaped with `<mark>` around matched terms (06 §6.3.4).
// Rather than injecting HTML, split them into text and highlight segments and
// let React render text nodes, so nothing else in a snippet can become markup.

export interface Segment {
  text: string;
  mark: boolean;
}

const ENTITIES: Record<string, string> = {
  "&amp;": "&",
  "&lt;": "<",
  "&gt;": ">",
  "&quot;": '"',
  "&#39;": "'",
  "&#x27;": "'",
};

function decode(s: string): string {
  return s.replace(/&(?:amp|lt|gt|quot|#39|#x27);/g, (e) => ENTITIES[e] ?? e);
}

export function snippetSegments(html: string): Segment[] {
  const out: Segment[] = [];
  const re = /<mark>([\s\S]*?)<\/mark>/g;
  let last = 0;
  for (let m = re.exec(html); m; m = re.exec(html)) {
    if (m.index > last) out.push({ text: decode(html.slice(last, m.index)), mark: false });
    out.push({ text: decode(m[1] ?? ""), mark: true });
    last = m.index + m[0].length;
  }
  if (last < html.length) out.push({ text: decode(html.slice(last)), mark: false });
  // Any other tag is shown literally, never interpreted.
  return out.filter((s) => s.text.length > 0);
}
