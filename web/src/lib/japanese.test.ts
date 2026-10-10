import { describe, expect, it } from "vitest";
import { hasJapanese } from "./japanese";

describe("hasJapanese", () => {
  it("matches kana, kanji and the API's other Japanese ranges", () => {
    for (const q of ["日本", "にほん", "ニュース", "ﾆｭｰｽ", "々", "Pearl 真珠湾", "𠮷"]) {
      expect(hasJapanese(q), q).toBe(true);
    }
  });
  it("leaves Latin text, digits and punctuation alone", () => {
    for (const q of ["pearl harbor", "1941", "café", '"cross of gold"', "「」"]) {
      expect(hasJapanese(q), q).toBe(false);
    }
  });
});
