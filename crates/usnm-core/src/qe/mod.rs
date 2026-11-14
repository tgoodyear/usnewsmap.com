//! Reference-free OCR quality measures, version 1: the out-of-lexicon rate,
//! a character 5-gram model's bits per character, their sum scaled by the
//! truth sets' spread (`lm_oov`), the omission guard's word count and a
//! no-words flag. The study that chose them, with the Python they are
//! ported from, is `docs/reports/2026-11-ocr-quality-estimation/`; its
//! `lm_oov` is the decision measure there, with the omission guard.
//!
//! Each language has one resource file ([`artifact`]), built offline by
//! `scripts/qe/build_resources.py` and `usnm-boinc quality qe-pack`: the
//! lexicon (wordfreq's whole list plus the Hunspell dictionary's forms, or
//! wordfreq alone where the only Hunspell licence is the GPL) and the
//! character model (interpolated Witten-Bell, trained on Gutenberg books
//! topped up from Wikisource, plus a wordfreq sample). Nothing in either was
//! made from any reader's output.
//!
//! Every measure is oriented so that higher is worse. The arithmetic is the
//! study's, with `libm`'s `log2`, so every platform gives the same bits.
mod artifact;
mod text;

pub use artifact::{pack, pruned, unpack, Entry, Header, PackOptions, Parts, MAX_BYTES};
pub use text::{fold, lm_text, normalize, words};

/// The measure's version, in every table and result that carries it.
pub const VERSION: &str = "qe-v1";
/// The pooled truth-set standard deviations `lm_oov` divides by.
pub const BPC_SD: f64 = 1.185;
pub const OOV_SD: f64 = 0.117;
/// The omission guard: a reading with fewer than this share of the other
/// reading's in-lexicon words of [`GUARD_MIN_LETTERS`] or more letters loses.
pub const GUARD_SHARE: f64 = 0.8;
pub const GUARD_MIN_LETTERS: usize = 3;

/// One reading's measures.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Score {
    /// Words (letter runs, inner apostrophes kept) after hyphen joining.
    pub words: u64,
    /// Of them, in the lexicon.
    pub known: u64,
    /// In the lexicon and of three or more letters: the omission guard's count.
    pub known3: u64,
    /// Characters the model scored.
    pub chars: u64,
    /// The reading has no word at all.
    pub no_words: bool,
    /// Out-of-lexicon share of the words; none without words.
    pub oov: Option<f64>,
    /// Bits per character; none without characters.
    pub charlm_bpc: Option<f64>,
    /// `charlm_bpc / BPC_SD + oov / OOV_SD`; none unless both are.
    pub lm_oov: Option<f64>,
}

/// One language's lexicon and character model.
pub struct Resources {
    header: Header,
    notice: String,
    lexicon: fst::Set<Vec<u8>>,
    model: fst::Map<Vec<u8>>,
}

impl Resources {
    /// Reads and checks a resource file.
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        let p = unpack(bytes)?;
        Ok(Self {
            header: p.header,
            notice: p.notice,
            lexicon: p.lexicon,
            model: p.model,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The licences and sources of the file, as shipped.
    pub fn notice(&self) -> &str {
        &self.notice
    }

    /// ISO 639-3.
    pub fn language(&self) -> &str {
        &self.header.language
    }

    /// Whether a folded word is in the lexicon.
    pub fn known(&self, folded: &str) -> bool {
        self.lexicon.contains(folded)
    }

    /// `(count, continuations, distinct continuations)` of an n-gram.
    fn entry(&self, key: &str) -> (u32, u32, u32) {
        self.model
            .get(key)
            .map_or((0, 0, 0), artifact::unpack_value)
    }

    /// Total log2 probability of an `lm_text` and the characters scored.
    pub fn logprob(&self, lm_text: &str) -> (f64, u64) {
        let n = self.header.order as usize;
        let mut cs: Vec<char> = vec!['\u{2}'; n - 1];
        cs.extend(lm_text.chars());
        let base = 1.0 / f64::from(self.header.vocab);
        let mut lp = 0.0;
        let mut key = String::new();
        for i in n - 1..cs.len() {
            let mut p = base;
            for k in 1..=n {
                key.clear();
                key.extend(&cs[i + 1 - k..i]);
                let (_, ctx, types) = self.entry(&key);
                if ctx == 0 {
                    break;
                }
                key.push(cs[i]);
                let (count, _, _) = self.entry(&key);
                p = (f64::from(count) + f64::from(types) * p) / f64::from(ctx + types);
            }
            lp += libm::log2(p);
        }
        (lp, (cs.len() - (n - 1)) as u64)
    }

    /// The measures of one reading.
    pub fn score(&self, reading: &str) -> Score {
        let lang = self.language();
        let normalized = normalize(reading);
        let (mut n, mut known, mut known3) = (0u64, 0u64, 0u64);
        for w in words(&normalized) {
            let f = fold(w, lang);
            n += 1;
            if self.known(&f) {
                known += 1;
                if f.chars().count() >= GUARD_MIN_LETTERS {
                    known3 += 1;
                }
            }
        }
        let lt = lm_text(reading, lang);
        let (lp, chars) = if lt.is_empty() {
            (0.0, 0)
        } else {
            self.logprob(&lt)
        };
        let oov = (n > 0).then(|| 1.0 - known as f64 / n as f64);
        let charlm_bpc = (chars > 0).then(|| -lp / chars as f64);
        let lm_oov = match (charlm_bpc, oov) {
            (Some(b), Some(o)) => Some(b / BPC_SD + o / OOV_SD),
            _ => None,
        };
        Score {
            words: n,
            known,
            known3,
            chars,
            no_words: n == 0,
            oov,
            charlm_bpc,
            lm_oov,
        }
    }
}

/// The study's model counts (`CharLM.train`) of one training text, already
/// in `lm_text` form, and the vocabulary size: every n-gram of 1 to `order`
/// characters over the text padded with `order - 1` U+0002, and for every
/// history the total and the number of distinct continuations. The shipped
/// models are trained by `scripts/qe/build_resources.py`; this serves tests
/// and small models.
pub fn counts(text: &str, order: usize) -> (Vec<Entry>, u32) {
    use std::collections::{BTreeMap, BTreeSet};
    let mut cs: Vec<char> = vec!['\u{2}'; order - 1];
    cs.extend(text.chars());
    let vocab = cs.iter().collect::<BTreeSet<_>>().len() as u32 + 1;
    let mut cnt: BTreeMap<String, u32> = BTreeMap::new();
    for i in order - 1..cs.len() {
        for k in 1..=order {
            *cnt.entry(cs[i + 1 - k..=i].iter().collect()).or_default() += 1;
        }
    }
    let mut hist: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for (g, c) in &cnt {
        let mut h: Vec<char> = g.chars().collect();
        h.pop();
        let e = hist.entry(h.into_iter().collect()).or_default();
        e.0 += c;
        e.1 += 1;
    }
    let keys: BTreeSet<&String> = cnt.keys().chain(hist.keys()).collect();
    let entries = keys
        .into_iter()
        .map(|k| {
            let (ctx, types) = hist.get(k).copied().unwrap_or_default();
            Entry {
                key: k.clone(),
                count: cnt.get(k).copied().unwrap_or_default(),
                ctx,
                types,
            }
        })
        .collect();
    (entries, vocab)
}

/// The study's pair decision on two readings of the same text: the omission
/// guard first, then the lower `lm_oov`. `Less` prefers `a`; none for a tie
/// or a missing value.
pub fn prefer(a: &Score, b: &Score) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering::{Greater, Less};
    if (a.known3 as f64) < GUARD_SHARE * b.known3 as f64 {
        return Some(Greater);
    }
    if (b.known3 as f64) < GUARD_SHARE * a.known3 as f64 {
        return Some(Less);
    }
    let (x, y) = (a.lm_oov?, b.lm_oov?);
    if x < y {
        Some(Less)
    } else if y < x {
        Some(Greater)
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
