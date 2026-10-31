// Comment markers inside JSON strings stay; malformed comments/strings fail closed.
export function parseJsonc(source) {
  let out = "";
  for (let i = 0; i < source.length; i++) {
    const c = source[i];
    if (c === '"') {
      let j = i + 1;
      while (j < source.length && source[j] !== '"') j += source[j] === "\\" ? 2 : 1;
      if (j >= source.length) throw new SyntaxError(`unterminated string starting at offset ${i}`);
      out += source.slice(i, j + 1);
      i = j;
    } else if (c === "/" && source[i + 1] === "/") {
      while (i < source.length && source[i] !== "\n") i++;
      out += "\n";
    } else if (c === "/" && source[i + 1] === "*") {
      const end = source.indexOf("*/", i + 2);
      if (end === -1) throw new SyntaxError(`unterminated block comment starting at offset ${i}`);
      i = end + 1;
    } else {
      out += c;
    }
  }
  return JSON.parse(out);
}
