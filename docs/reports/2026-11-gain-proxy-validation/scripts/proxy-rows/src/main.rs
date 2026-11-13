//! One row in, one row out: the production damage rate of a reading and its CER against the reference.
//!
//!   proxy-rows < in.jsonl > out.jsonl
//!
//! In: {"id", "lang" (ISO 639-3), "ref", "hyp"}. Out: {"id", "ref_chars", "char_edits", "folded_edits",
//! "hyp_head", "hyp_near", "hyp_rate", "hyp_words", "hyp_known", "ref_words", "ref_known", "ref_head", "ref_near", "ref_rate"}.
//! Damage is `usnm_core::damage::score_forced`, the function the OCR survey and the quality feed use, run
//! on the reading's raw text. CER is `usnm_ocr_survey::score::cer_wer`, the one `gpu-study score` uses;
//! "folded_edits" folds long s, the double hyphen and superscript e on both sides first, as
//! `gpu-study score --fold` does.
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use usnm_ocr_survey::score::cer_wer;

fn folded(s: &str) -> String {
    s.replace('ſ', "s").replace('⸗', "-").replace('\u{2E17}', "-").replace('\u{0364}', "\u{0308}").replace('ʒ', "z").replace('¬', "-")
}

fn dmg(text: &str, lang: &str) -> (Value, Value, Value, Value, Value) {
    match usnm_core::damage::score_forced(text, lang) {
        Some(s) => (json!(s.damage.head), json!(s.damage.near), json!(s.damage_rate), json!(s.damage.words), json!(s.known_words)),
        None => (Value::Null, Value::Null, Value::Null, Value::Null, Value::Null),
    }
}

fn main() {
    let out = std::io::stdout();
    let mut out = out.lock();
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).unwrap();
        let (r, h, lang) = (v["ref"].as_str().unwrap_or(""), v["hyp"].as_str().unwrap_or(""), v["lang"].as_str().unwrap());
        let c = cer_wer(r, h);
        let f = cer_wer(&folded(r), &folded(h));
        let (hh, hn, hr, hw, hk) = dmg(h, lang);
        let (rh, rn, rr, rw, rk) = dmg(r, lang);
        let row = json!({"id": v["id"], "ref_chars": c.ref_chars, "char_edits": c.char_edits, "folded_edits": f.char_edits,
            "hyp_head": hh, "hyp_near": hn, "hyp_rate": hr, "hyp_words": hw, "hyp_known": hk, "ref_words": rw, "ref_known": rk, "ref_head": rh, "ref_near": rn, "ref_rate": rr});
        writeln!(out, "{}", row).unwrap();
    }
}
