//! The synthetic corpus the measurements index (`decade_measure`,
//! `split_latency`): pages in archive order (batches of titles, each title's
//! pages by date, decades weighted like production's), their words drawn
//! from a vocabulary with Zipf frequencies. Deterministic, so every run
//! indexes the same pages.

// Each measurement uses part of it.
#![allow(dead_code)]

use serde_json::{json, Value};

/// Pages per decade in production (`baseline_pages`, October 2026, in
/// thousands), the decades before 1820 in the 1820s.
pub const WEIGHTS: [(u16, u32); 15] = [
    (1820, 193),
    (1830, 113),
    (1840, 252),
    (1850, 499),
    (1860, 734),
    (1870, 1013),
    (1880, 1526),
    (1890, 2909),
    (1900, 4531),
    (1910, 5590),
    (1920, 2832),
    (1930, 1241),
    (1940, 1146),
    (1950, 814),
    (1960, 329),
];

const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// xorshift64*: deterministic, so every run indexes the same corpus.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// A vocabulary with Zipf frequencies: the common words first, then made-up
/// words, like OCR text's long tail.
struct Words {
    words: Vec<String>,
    cumulative: Vec<f64>,
}

impl Words {
    fn new(rng: &mut Rng) -> Self {
        let mut words: Vec<String> = usnm_core::common_grams::WORDS
            .iter()
            .map(|w| (*w).to_owned())
            .collect();
        const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
        while words.len() < 60_000 {
            let len = 3 + rng.below(8) as usize;
            words.push(
                (0..len)
                    .map(|_| LETTERS[rng.below(26) as usize] as char)
                    .collect(),
            );
        }
        let mut total = 0.0;
        let cumulative = (1..=words.len())
            .map(|rank| {
                total += 1.0 / rank as f64;
                total
            })
            .collect();
        Self { words, cumulative }
    }

    fn pick(&self, rng: &mut Rng) -> &str {
        let last = *self.cumulative.last().unwrap();
        let x = (rng.next() >> 11) as f64 / (1u64 << 53) as f64 * last;
        let i = self.cumulative.partition_point(|c| *c < x);
        &self.words[i.min(self.words.len() - 1)]
    }
}

/// The vocabulary's word at `rank`, 0 the most frequent. The first ranks
/// are `usnm_core::common_grams::WORDS`; past them come made-up words, each
/// drawn in proportion to 1 / (rank + 1).
pub fn word(rank: usize) -> String {
    static VOCABULARY: std::sync::OnceLock<Words> = std::sync::OnceLock::new();
    VOCABULARY.get_or_init(|| Words::new(&mut Rng(SEED))).words[rank].clone()
}

/// What [`corpus`] makes.
pub struct Corpus {
    pub docs: u64,
    /// Mean words a page (each page 70% to 130% of it).
    pub words_per_page: u64,
    /// Each page's `decade` field.
    pub with_decade: bool,
    /// Pages a title has in its batch: at least the first, fewer than the
    /// second.
    pub title_pages: (u64, u64),
    /// Text added to some pages: `(text, every)` ends every `every`th page
    /// with `text`. The draws, and so the rest of the corpus, don't change.
    pub planted: Vec<(String, u64)>,
}

/// The corpus in archive order: batches of 2 to 6 titles, each title's run
/// 3 to 15 years from a decade drawn by production's weights, its pages by
/// date. `f` gets each page's document.
pub fn corpus(c: &Corpus, mut f: impl FnMut(Value)) {
    let (fewest, most) = c.title_pages;
    assert!(
        0 < fewest && fewest < most,
        "title_pages {:?}: need 0 < fewest < most",
        c.title_pages
    );
    let mut rng = Rng(SEED);
    let words = Words::new(&mut rng);
    let total: u32 = WEIGHTS.iter().map(|(_, w)| w).sum();
    let mut made = 0u64;
    let mut title = 0u32;
    let mut batch = 0u32;
    while made < c.docs {
        batch += 1;
        for _ in 0..2 + rng.below(5) {
            title += 1;
            let mut pick = rng.below(u64::from(total)) as u32;
            let decade = WEIGHTS
                .iter()
                .find(|(_, w)| {
                    let hit = pick < *w;
                    pick = pick.saturating_sub(*w);
                    hit
                })
                .map_or(1900, |(d, _)| *d);
            let start = i32::from(decade) + rng.below(10) as i32;
            let years = 3 + rng.below(13) as i32;
            let pages = fewest + rng.below(most - fewest);
            let days = (years * 365) as u64;
            for n in 0..pages {
                if made >= c.docs {
                    return;
                }
                let date = chrono::NaiveDate::from_ymd_opt(start, 1, 1).unwrap()
                    + chrono::Duration::days((n * days / pages) as i64);
                let text: Vec<&str> = (0..c.words_per_page * 7 / 10
                    + rng.below(c.words_per_page * 6 / 10))
                    .map(|_| words.pick(&mut rng))
                    .collect();
                let mut text = text.join(" ");
                for (planted, every) in &c.planted {
                    if (made + 1).is_multiple_of(*every) {
                        text.push(' ');
                        text.push_str(planted);
                    }
                }
                let lccn = format!("sn{:08}", title);
                let mut doc = json!({
                    "doc_id": format!("{lccn}_{date}_ed-1_seq-{}", n % 8 + 1),
                    "day": usnm_core::time::day_number(date),
                    "ym": usnm_core::time::ym_number(date),
                    "year": chrono::Datelike::year(&date),
                    "place_id": format!("P{:05}", title % 2000),
                    "place_shard": title % 8,
                    "lccn": lccn,
                    "state": "IL",
                    "language": ["eng"],
                    "front_page": n % 8 == 0,
                    "edition": 1,
                    "seq": n % 8 + 1,
                    "sort_key": (u64::from(title) << 32) | (n % 8 + 1),
                    "date": date.to_string(),
                    "batch": format!("batch_measure_{batch:04}_ver01"),
                    "text_cg": usnm_core::common_grams::index_text(&text, usnm_core::text::Analyzer::LATEST),
                    "text": text,
                });
                if c.with_decade {
                    doc["decade"] = usnm_core::decade::of_date(date).into();
                }
                f(doc);
                made += 1;
            }
        }
    }
}
