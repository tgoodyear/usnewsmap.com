//! Japanese text for search (#139): our own OCR of the Japanese pages LoC has
//! no text for (04 §4.8) is indexed as one token per Japanese character,
//! because Japanese has no spaces between words and Quickwit 0.9.1's
//! `chinese_compatible` tokenizer splits only Han, not kana. Folding happens
//! here, on both sides (index and query), so they always agree:
//!
//! - old character forms to modern ones (戰 → 戦, 國 → 国), since prewar print
//!   uses the old forms and people search with the new;
//! - small kana to large (っ → つ), which prewar print often doesn't distinguish;
//! - CJK compatibility ideographs to their unified forms (NFC).
//!
//! Every fold maps one character to one character, so offsets in the folded
//! text are offsets in the printed text: snippets show the forms as printed.
//! Latin runs on a Japanese page fold exactly as the main index folds them
//! ([`crate::text::fold`], with the same [`Analyzer`]), and a string with no
//! Japanese tokenizes exactly as [`crate::text::tokenize`] does.

use crate::text::{fold, Analyzer, MAX_TOKEN_CHARS};
use unicode_normalization::UnicodeNormalization;

/// The latest version of the folding, which a full rebuild builds with:
/// bumped whenever the folding changes, since the index and the API must
/// agree (`current.json`'s `ja.fold`). 1: the first. 2: Latin runs keep `½`
/// and the like as one word (#168, [`Analyzer::V2`]).
pub const FOLD_VERSION: u32 = 2;

/// The analyzer of fold version `version`, or `None` for a version this
/// code doesn't know (a newer one).
pub fn analyzer(version: u32) -> Option<Analyzer> {
    match version {
        1 => Some(Analyzer::V1),
        2 => Some(Analyzer::V2),
        _ => None,
    }
}

/// The fold version an index built with `analyzer` records.
pub fn fold_version(analyzer: Analyzer) -> u32 {
    match analyzer {
        Analyzer::V1 => 1,
        Analyzer::V2 => 2,
    }
}

/// Characters a Japanese query run may have (a run is one phrase).
pub const MAX_JA_RUN_CHARS: usize = 32;

/// Kana, Han and the Japanese iteration and length marks.
pub fn is_ja(c: char) -> bool {
    matches!(c,
        '\u{3005}'..='\u{3007}'     // 々 〆 〇
        | '\u{3041}'..='\u{3096}'   // hiragana
        | '\u{309D}'..='\u{309F}'   // ゝ ゞ ゟ
        | '\u{30A1}'..='\u{30FA}'   // katakana
        | '\u{30FC}'..='\u{30FF}'   // ー ヽ ヾ ヿ
        | '\u{31F0}'..='\u{31FF}'   // katakana phonetic extensions
        | '\u{3400}'..='\u{4DBF}'   // CJK extension A
        | '\u{4E00}'..='\u{9FFF}'   // CJK unified ideographs
        | '\u{F900}'..='\u{FAFF}'   // CJK compatibility ideographs
        | '\u{FF66}'..='\u{FF9F}'   // halfwidth katakana
        | '\u{20000}'..='\u{2FA1F}' // CJK extensions B–F, compatibility supplement
    )
}

/// Old form → modern form, one character each (Tōyō/Jōyō and Jinmeiyō
/// simplifications). Sorted by the old form for binary search; a test checks.
const OLD_TO_NEW: &[(char, char)] = &[
    ('乘', '乗'),
    ('亂', '乱'),
    ('亞', '亜'),
    ('佛', '仏'),
    ('來', '来'),
    ('傳', '伝'),
    ('價', '価'),
    ('儉', '倹'),
    ('兒', '児'),
    ('兩', '両'),
    ('剩', '剰'),
    ('劍', '剣'),
    ('劑', '剤'),
    ('勞', '労'),
    ('勳', '勲'),
    ('勵', '励'),
    ('區', '区'),
    ('卷', '巻'),
    ('卽', '即'),
    ('參', '参'),
    ('囘', '回'),
    ('圈', '圏'),
    ('國', '国'),
    ('圍', '囲'),
    ('圓', '円'),
    ('圖', '図'),
    ('團', '団'),
    ('墮', '堕'),
    ('壓', '圧'),
    ('壘', '塁'),
    ('壞', '壊'),
    ('壯', '壮'),
    ('壹', '壱'),
    ('壽', '寿'),
    ('奧', '奥'),
    ('奬', '奨'),
    ('孃', '嬢'),
    ('學', '学'),
    ('寢', '寝'),
    ('實', '実'),
    ('寫', '写'),
    ('寬', '寛'),
    ('寶', '宝'),
    ('將', '将'),
    ('專', '専'),
    ('對', '対'),
    ('屆', '届'),
    ('屬', '属'),
    ('峽', '峡'),
    ('嶽', '岳'),
    ('帶', '帯'),
    ('廢', '廃'),
    ('廣', '広'),
    ('廳', '庁'),
    ('彈', '弾'),
    ('徑', '径'),
    ('從', '従'),
    ('徵', '徴'),
    ('德', '徳'),
    ('應', '応'),
    ('戀', '恋'),
    ('戰', '戦'),
    ('戲', '戯'),
    ('拂', '払'),
    ('拔', '抜'),
    ('拜', '拝'),
    ('挾', '挟'),
    ('插', '挿'),
    ('揭', '掲'),
    ('搖', '揺'),
    ('搜', '捜'),
    ('擇', '択'),
    ('擊', '撃'),
    ('擔', '担'),
    ('據', '拠'),
    ('擧', '挙'),
    ('擴', '拡'),
    ('攝', '摂'),
    ('收', '収'),
    ('敍', '叙'),
    ('敎', '教'),
    ('數', '数'),
    ('斷', '断'),
    ('晉', '晋'),
    ('晝', '昼'),
    ('曆', '暦'),
    ('曉', '暁'),
    ('會', '会'),
    ('條', '条'),
    ('棧', '桟'),
    ('樂', '楽'),
    ('樓', '楼'),
    ('樞', '枢'),
    ('樣', '様'),
    ('檢', '検'),
    ('權', '権'),
    ('歐', '欧'),
    ('歡', '歓'),
    ('步', '歩'),
    ('歲', '歳'),
    ('歷', '歴'),
    ('歸', '帰'),
    ('殘', '残'),
    ('殼', '殻'),
    ('毆', '殴'),
    ('氣', '気'),
    ('沒', '没'),
    ('淚', '涙'),
    ('淨', '浄'),
    ('淺', '浅'),
    ('渴', '渇'),
    ('溪', '渓'),
    ('滯', '滞'),
    ('滿', '満'),
    ('潛', '潜'),
    ('澁', '渋'),
    ('澤', '沢'),
    ('濕', '湿'),
    ('濟', '済'),
    ('濱', '浜'),
    ('瀧', '滝'),
    ('灣', '湾'),
    ('燈', '灯'),
    ('燒', '焼'),
    ('營', '営'),
    ('爐', '炉'),
    ('爭', '争'),
    ('爲', '為'),
    ('犧', '犠'),
    ('狀', '状'),
    ('狹', '狭'),
    ('獨', '独'),
    ('獵', '猟'),
    ('獸', '獣'),
    ('獻', '献'),
    ('瓣', '弁'),
    ('甁', '瓶'),
    ('產', '産'),
    ('畫', '画'),
    ('當', '当'),
    ('疊', '畳'),
    ('癡', '痴'),
    ('發', '発'),
    ('盜', '盗'),
    ('盡', '尽'),
    ('眞', '真'),
    ('硏', '研'),
    ('碎', '砕'),
    ('祕', '秘'),
    ('禪', '禅'),
    ('禮', '礼'),
    ('稱', '称'),
    ('稻', '稲'),
    ('穩', '穏'),
    ('竊', '窃'),
    ('竝', '並'),
    ('粹', '粋'),
    ('絲', '糸'),
    ('經', '経'),
    ('綠', '緑'),
    ('緖', '緒'),
    ('縣', '県'),
    ('縱', '縦'),
    ('總', '総'),
    ('繩', '縄'),
    ('繪', '絵'),
    ('繼', '継'),
    ('續', '続'),
    ('纖', '繊'),
    ('缺', '欠'),
    ('聰', '聡'),
    ('聲', '声'),
    ('聽', '聴'),
    ('肅', '粛'),
    ('脫', '脱'),
    ('腦', '脳'),
    ('膽', '胆'),
    ('臟', '臓'),
    ('臺', '台'),
    ('與', '与'),
    ('舊', '旧'),
    ('舍', '舎'),
    ('莊', '荘'),
    ('莖', '茎'),
    ('萬', '万'),
    ('藏', '蔵'),
    ('藝', '芸'),
    ('藥', '薬'),
    ('處', '処'),
    ('號', '号'),
    ('螢', '蛍'),
    ('蟲', '虫'),
    ('蠶', '蚕'),
    ('蠻', '蛮'),
    ('衞', '衛'),
    ('裝', '装'),
    ('襃', '褒'),
    ('覺', '覚'),
    ('覽', '覧'),
    ('觀', '観'),
    ('觸', '触'),
    ('證', '証'),
    ('譯', '訳'),
    ('譽', '誉'),
    ('讀', '読'),
    ('變', '変'),
    ('讓', '譲'),
    ('豐', '豊'),
    ('豫', '予'),
    ('貳', '弐'),
    ('賣', '売'),
    ('賴', '頼'),
    ('贊', '賛'),
    ('踐', '践'),
    ('輕', '軽'),
    ('轉', '転'),
    ('辨', '弁'),
    ('辭', '辞'),
    ('辯', '弁'),
    ('遞', '逓'),
    ('遲', '遅'),
    ('邊', '辺'),
    ('鄕', '郷'),
    ('醉', '酔'),
    ('醫', '医'),
    ('醬', '醤'),
    ('釀', '醸'),
    ('釋', '釈'),
    ('錄', '録'),
    ('錢', '銭'),
    ('鎭', '鎮'),
    ('鐵', '鉄'),
    ('鑄', '鋳'),
    ('鑛', '鉱'),
    ('關', '関'),
    ('陷', '陥'),
    ('隨', '随'),
    ('險', '険'),
    ('隱', '隠'),
    ('隸', '隷'),
    ('雙', '双'),
    ('雜', '雑'),
    ('雞', '鶏'),
    ('靈', '霊'),
    ('靜', '静'),
    ('顯', '顕'),
    ('飮', '飲'),
    ('餘', '余'),
    ('騷', '騒'),
    ('驅', '駆'),
    ('驗', '験'),
    ('驛', '駅'),
    ('髓', '髄'),
    ('髮', '髪'),
    ('鬪', '闘'),
    ('鷄', '鶏'),
    ('鹽', '塩'),
    ('麥', '麦'),
    ('黃', '黄'),
    ('黑', '黒'),
    ('默', '黙'),
    ('點', '点'),
    ('黨', '党'),
    ('齊', '斉'),
    ('齋', '斎'),
    ('齒', '歯'),
    ('齡', '齢'),
    ('龍', '竜'),
];

const SMALL_KANA: &[(char, char)] = &[
    ('ぁ', 'あ'),
    ('ぃ', 'い'),
    ('ぅ', 'う'),
    ('ぇ', 'え'),
    ('ぉ', 'お'),
    ('っ', 'つ'),
    ('ゃ', 'や'),
    ('ゅ', 'ゆ'),
    ('ょ', 'よ'),
    ('ゎ', 'わ'),
    ('ゕ', 'か'),
    ('ゖ', 'け'),
    ('ァ', 'ア'),
    ('ィ', 'イ'),
    ('ゥ', 'ウ'),
    ('ェ', 'エ'),
    ('ォ', 'オ'),
    ('ッ', 'ツ'),
    ('ャ', 'ヤ'),
    ('ュ', 'ユ'),
    ('ョ', 'ヨ'),
    ('ヮ', 'ワ'),
    ('ヵ', 'カ'),
    ('ヶ', 'ケ'),
];

/// Fold one Japanese character (see the module docs). Always one character.
pub fn fold_char(c: char) -> char {
    // NFC maps compatibility ideographs to unified ones; NFKC maps halfwidth
    // katakana to fullwidth. Both are one character here; keep `c` otherwise.
    let c = if ('\u{FF66}'..='\u{FF9F}').contains(&c) {
        single(c.to_string().nfkc().collect::<String>()).unwrap_or(c)
    } else {
        single(c.to_string().nfc().collect::<String>()).unwrap_or(c)
    };
    if let Ok(i) = OLD_TO_NEW.binary_search_by_key(&c, |&(old, _)| old) {
        return OLD_TO_NEW[i].1;
    }
    SMALL_KANA
        .iter()
        .find(|&&(small, _)| small == c)
        .map_or(c, |&(_, large)| large)
}

/// Voiced and semi-voiced sound marks, combining and halfwidth.
pub fn is_voicing_mark(c: char) -> bool {
    matches!(c, '\u{3099}' | '\u{309A}' | '\u{FF9E}' | '\u{FF9F}')
}

fn single(s: String) -> Option<char> {
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// A search token and where it came from in the text, in characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub ja: bool,
}

/// Split text into search tokens: one per Japanese character (folded), and
/// Latin and digit runs folded as the main index folds them with `analyzer`.
/// With no Japanese in `text`, the token texts equal
/// [`crate::text::tokenize`]'s.
pub fn tokens(text: &str, analyzer: Analyzer) -> Vec<Token> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if !c.is_alphanumeric() {
            i += 1;
        } else if is_voicing_mark(c) {
            // A voicing mark with nothing to combine with isn't searchable.
            i += 1;
        } else if is_ja(c) {
            // A kana followed by a separate voicing mark (halfwidth ｶﾞ, or
            // decomposed か+゙) is one character: compose them first.
            let (c, width) = match chars.get(i + 1) {
                Some(&m) if is_voicing_mark(m) => {
                    match single([c, m].iter().collect::<String>().nfkc().collect()) {
                        Some(composed) => (composed, 2),
                        None => (c, 1),
                    }
                }
                _ => (c, 1),
            };
            out.push(Token {
                text: fold_char(c).to_string(),
                start: i,
                end: i + width,
                ja: true,
            });
            i += width;
        } else {
            let start = i;
            while i < chars.len() && chars[i].is_alphanumeric() && !is_ja(chars[i]) {
                i += 1;
            }
            let t = fold(&chars[start..i].iter().collect::<String>(), analyzer);
            if !t.is_empty() && t.chars().count() <= MAX_TOKEN_CHARS {
                out.push(Token {
                    text: t,
                    start,
                    end: i,
                    ja: false,
                });
            }
        }
    }
    out
}

/// The token texts of [`tokens`].
pub fn tokenize(text: &str, analyzer: Analyzer) -> Vec<String> {
    tokens(text, analyzer).into_iter().map(|t| t.text).collect()
}

/// What a Japanese page's `text` field holds: its tokens joined by spaces, for
/// Quickwit's `whitespace` tokenizer (which keeps positions, so phrases work).
pub fn index_text(printed: &str, analyzer: Analyzer) -> String {
    tokenize(printed, analyzer).join(" ")
}

/// Whether a string has any Japanese character.
pub fn has_ja(s: &str) -> bool {
    s.chars().any(is_ja)
}

/// Where `phrases` (each a token sequence, as the query parser builds them)
/// occur in `printed`, as character ranges, sorted and merged.
pub fn find(printed: &str, phrases: &[Vec<String>], analyzer: Analyzer) -> Vec<(usize, usize)> {
    let toks = tokens(printed, analyzer);
    let mut hits = Vec::new();
    for phrase in phrases.iter().filter(|p| !p.is_empty()) {
        for w in toks.windows(phrase.len()) {
            if w.iter().zip(phrase).all(|(t, p)| &t.text == p) {
                hits.push((w[0].start, w[phrase.len() - 1].end));
            }
        }
    }
    hits.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in hits {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

/// Up to `max` snippets of `printed` around the matches of `phrases`: each a
/// list of (is_match, text) pieces, with about `context` characters either side.
/// Line breaks (columns) become spaces.
pub fn snippets(
    printed: &str,
    phrases: &[Vec<String>],
    context: usize,
    max: usize,
    analyzer: Analyzer,
) -> Vec<Vec<(bool, String)>> {
    let chars: Vec<char> = printed
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    let mut out = Vec::new();
    let mut covered = 0;
    let hits = find(printed, phrases, analyzer);
    let mut k = 0;
    while k < hits.len() && out.len() < max {
        let (s, _) = hits[k];
        if s < covered {
            k += 1;
            continue;
        }
        let from = s.saturating_sub(context).max(covered);
        let to_limit = (s + context * 2).min(chars.len());
        let mut pieces = Vec::new();
        let mut at = from;
        while k < hits.len() && hits[k].0 < to_limit {
            let (hs, he) = hits[k];
            if hs > at {
                pieces.push((false, chars[at..hs].iter().collect()));
            }
            pieces.push((true, chars[hs..he].iter().collect()));
            at = he;
            k += 1;
        }
        let to = (at + context).min(chars.len()).max(at);
        if to > at {
            pieces.push((false, chars[at..to].iter().collect()));
        }
        covered = to;
        out.push(pieces);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text;

    const A: Analyzer = Analyzer::LATEST;

    #[test]
    fn the_fold_table_is_sorted_one_to_one_and_folds_to_modern_forms() {
        for w in OLD_TO_NEW.windows(2) {
            assert!(w[0].0 < w[1].0, "not sorted at {:?} {:?}", w[0], w[1]);
        }
        for &(old, new) in OLD_TO_NEW {
            assert!(is_ja(old) && is_ja(new), "{old} {new}");
            // A modern form is never itself folded again.
            assert!(
                OLD_TO_NEW.binary_search_by_key(&new, |&(o, _)| o).is_err() || old == new,
                "{new}"
            );
        }
    }

    #[test]
    fn folds_old_forms_small_kana_and_compatibility_ideographs() {
        assert_eq!(fold_char('戰'), '戦');
        assert_eq!(fold_char('國'), '国');
        assert_eq!(fold_char('戦'), '戦');
        assert_eq!(fold_char('っ'), 'つ');
        assert_eq!(fold_char('ャ'), 'ヤ');
        assert_eq!(fold_char('ｶ'), 'カ');
        assert_eq!(fold_char('\u{F91D}'), '欄'); // compatibility ideograph
        assert_eq!(fold_char('が'), 'が'); // dakuten kept (text::fold would strip it)
    }

    #[test]
    fn halfwidth_and_decomposed_voiced_kana_compose() {
        assert_eq!(tokenize("ｶﾞｽ", A), vec!["ガ", "ス"]);
        assert_eq!(tokenize("ガス", A), vec!["ガ", "ス"]);
        assert_eq!(tokenize("か\u{3099}", A), vec!["が"]);
        assert_eq!(tokenize("ﾊﾟﾝ", A), vec!["パ", "ン"]);
        // The composed token covers both printed characters.
        let t = tokens("ｶﾞｽ", A);
        assert_eq!((t[0].start, t[0].end, t[1].start), (0, 2, 2));
        // A stray mark is dropped.
        assert_eq!(tokenize("\u{FF9E}ス", A), vec!["ス"]);
    }

    #[test]
    fn tokens_one_per_japanese_character_and_latin_runs_as_the_main_index() {
        assert_eq!(
            tokenize("去年の大記事は何?やはり西歐大侵略戰", A),
            "去 年 の 大 記 事 は 何 や は り 西 欧 大 侵 略 戦"
                .split(' ')
                .collect::<Vec<_>>()
        );
        assert_eq!(
            tokenize("ROCKY Shimpo 新報 1945年", A),
            vec!["rocky", "shimpo", "新", "報", "1945", "年"]
        );
        let t = tokens("A去年", A);
        assert_eq!((t[1].start, t[1].end, t[1].ja), (1, 2, true));
    }

    #[test]
    fn without_japanese_it_tokenizes_exactly_like_the_main_index() {
        for s in [
            "Crucify mankind upon a CROSS of Gold! Café—ſo",
            "well-known 1896-97 Æsop ﬁne naïve",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa short",
            "Ñoño Łódź Straße 12th",
            "Wheat ½ higher at 61¼ ﷺ Ŀa",
        ] {
            for a in Analyzer::ALL {
                assert_eq!(tokenize(s, a), text::tokenize(s, a), "{s} {a:?}");
            }
        }
        // Each version folds a Latin run on a Japanese page as the main
        // index of that version does (#168).
        assert_eq!(index_text("小麦 ½", Analyzer::V2), "小 麦 ½");
        assert_eq!(index_text("小麦 ½", Analyzer::V1), "小 麦 1\u{2044}2");
    }

    #[test]
    fn fold_versions_name_their_analyzers() {
        assert_eq!(analyzer(FOLD_VERSION), Some(Analyzer::LATEST));
        for a in Analyzer::ALL {
            assert_eq!(analyzer(fold_version(a)), Some(a));
        }
        assert_eq!(analyzer(FOLD_VERSION + 1), None);
    }

    #[test]
    fn index_text_is_space_separated_tokens() {
        assert_eq!(index_text("西歐大\n侵略戰", A), "西 欧 大 侵 略 戦");
    }

    #[test]
    fn finds_phrases_in_printed_text_through_folding() {
        let printed = "やはり西歐大侵略戰\n米國通信社";
        let p = vec![tokenize("西欧", A), tokenize("米国", A)];
        assert_eq!(find(printed, &p, A), vec![(3, 5), (10, 12)]);
        let s = snippets(printed, &[tokenize("侵略戦", A)], 3, 2, A);
        assert_eq!(
            s,
            vec![vec![
                (false, "西歐大".to_owned()),
                (true, "侵略戰".to_owned()),
                (false, " 米國".to_owned()),
            ]]
        );
    }
}
