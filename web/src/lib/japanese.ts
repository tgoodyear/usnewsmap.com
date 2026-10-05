/**
 * Japanese script, as the API routes a query (`usnm_core::ja::is_ja`): a query
 * with any of these characters searches only the Japanese pages we OCR'd
 * (#139). Keep the ranges in step with `crates/usnm-core/src/ja.rs`.
 */
const JA = /[々-〇ぁ-ゖゝ-ゟァ-ヺー-ヿㇰ-ㇿ㐀-䶿一-鿿豈-﫿ｦ-ﾟ\u{20000}-\u{2FA1F}]/u;

export function hasJapanese(s: string): boolean {
  return JA.test(s);
}
