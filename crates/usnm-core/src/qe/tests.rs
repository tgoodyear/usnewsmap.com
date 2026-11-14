use super::*;

/// `scripts/qe/golden.py`'s output: the study's own `qe_lib` on a small
/// model, lexicon and set of texts.
const GOLDEN: &str = include_str!("../../tests/data/qe-golden.json");

fn golden() -> serde_json::Value {
    serde_json::from_str(GOLDEN).unwrap()
}

fn resources(g: &serde_json::Value, prune_min_count: u32) -> Resources {
    let words = g["lexicon"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap().to_owned());
    let entries = g["entries"].as_array().unwrap().iter().map(|e| Entry {
        key: e[0].as_str().unwrap().to_owned(),
        count: e[1].as_u64().unwrap() as u32,
        ctx: e[2].as_u64().unwrap() as u32,
        types: e[3].as_u64().unwrap() as u32,
    });
    let bytes = pack(
        words,
        entries,
        &PackOptions {
            language: "eng",
            order: 5,
            vocab: g["vocab"].as_u64().unwrap() as u32,
            prune_min_count,
            prune_from_order: 3,
            hunspell: false,
            licence: "CC-BY-SA-4.0",
            notice: "test notice\n",
        },
    )
    .unwrap();
    Resources::from_bytes(&bytes).unwrap()
}

#[test]
fn text_handling_matches_the_study() {
    let g = golden();
    for c in g["cases"].as_array().unwrap() {
        let t = c["text"].as_str().unwrap();
        assert_eq!(normalize(t), c["normalized"].as_str().unwrap(), "{t:?}");
        let w: Vec<String> = words(&normalize(t))
            .into_iter()
            .map(|w| fold(w, "eng"))
            .collect();
        let want: Vec<&str> = c["words"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap())
            .collect();
        assert_eq!(w, want, "{t:?}");
        assert_eq!(lm_text(t, "eng"), c["lm_text"].as_str().unwrap(), "{t:?}");
    }
    for f in g["folds"].as_array().unwrap() {
        let (lang, t) = (f["lang"].as_str().unwrap(), f["text"].as_str().unwrap());
        assert_eq!(fold(t, lang), f["fold"].as_str().unwrap(), "{lang} {t:?}");
        assert_eq!(
            lm_text(t, lang),
            f["lm_text"].as_str().unwrap(),
            "{lang} {t:?}"
        );
        let w: Vec<String> = words(&normalize(t))
            .into_iter()
            .map(|w| fold(w, lang))
            .collect();
        let want: Vec<&str> = f["words"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap())
            .collect();
        assert_eq!(w, want, "{lang} {t:?}");
    }
}

#[test]
fn scores_match_the_study() {
    let g = golden();
    let r = resources(&g, 1);
    assert_eq!(r.notice(), "test notice\n");
    for c in g["cases"].as_array().unwrap() {
        let t = c["text"].as_str().unwrap();
        let s = r.score(t);
        let (lp, chars) = if c["lm_text"].as_str().unwrap().is_empty() {
            (0.0, 0)
        } else {
            r.logprob(c["lm_text"].as_str().unwrap())
        };
        assert_eq!(chars, c["chars"].as_u64().unwrap(), "{t:?}");
        assert!(
            (lp - c["logprob"].as_f64().unwrap()).abs() < 1e-9,
            "{t:?}: {lp}"
        );
        assert_eq!(s.known, c["known"].as_u64().unwrap(), "{t:?}");
        assert_eq!(s.known3, c["known3"].as_u64().unwrap(), "{t:?}");
        assert_eq!(s.oov, c["oov"].as_f64(), "{t:?}");
        match (s.charlm_bpc, c["bpc"].as_f64()) {
            (Some(a), Some(b)) => assert!((a - b).abs() < 1e-12, "{t:?}: {a} {b}"),
            (a, b) => assert_eq!(a, b, "{t:?}"),
        }
        assert_eq!(s.no_words, s.words == 0);
        assert_eq!(
            s.lm_oov,
            s.charlm_bpc
                .zip(s.oov)
                .map(|(b, o)| b / BPC_SD + o / OOV_SD)
        );
    }
}

#[test]
fn the_illegible_marker_has_no_words_but_a_model_score() {
    let r = resources(&golden(), 1);
    let s = r.score("[?]");
    assert!(s.no_words && s.oov.is_none() && s.lm_oov.is_none());
    assert!(s.charlm_bpc.is_some_and(|b| b > 0.0));
    let e = r.score("");
    assert!(e.no_words && e.charlm_bpc.is_none() && e.chars == 0);
}

#[test]
fn pruning_drops_rare_long_ngrams_only() {
    let g = golden();
    let full = resources(&g, 1);
    let pruned_ = resources(&g, 2);
    assert!(pruned_.header().model_entries < full.header().model_entries);
    for e in g["entries"].as_array().unwrap() {
        let key = e[0].as_str().unwrap();
        let count = e[1].as_u64().unwrap() as u32;
        let kept = pruned_.model.get(key).is_some();
        let dropped = pruned(
            &Entry {
                key: key.into(),
                count,
                ctx: 0,
                types: 0,
            },
            2,
            3,
        );
        assert_eq!(kept, !dropped, "{key:?}");
    }
    // A clean reading still scores better than a damaged one after pruning.
    let clean = pruned_.score("the mayor spoke with the council about the bridge");
    let damaged = pruned_.score("tbe rnayor spokc wiht tlie coimcil abont tbe bridgc");
    assert_eq!(prefer(&clean, &damaged), Some(std::cmp::Ordering::Less));
}

#[test]
fn the_guard_beats_the_rate() {
    let r = resources(&golden(), 1);
    let long = r.score("the mayor spoke with the council about the bridges and the river");
    let short = r.score("the mayor");
    assert_eq!(prefer(&short, &long), Some(std::cmp::Ordering::Greater));
    assert_eq!(prefer(&long, &short), Some(std::cmp::Ordering::Less));
    assert_eq!(prefer(&long, &long), None);
}

#[test]
fn damaged_files_are_refused() {
    let g = golden();
    let words = vec!["the".to_owned()];
    let entries = vec![Entry {
        key: String::new(),
        count: 0,
        ctx: 3,
        types: 1,
    }];
    let opts = PackOptions {
        language: "eng",
        order: 5,
        vocab: g["vocab"].as_u64().unwrap() as u32,
        prune_min_count: 1,
        prune_from_order: 3,
        hunspell: false,
        licence: "CC-BY-SA-4.0",
        notice: "n",
    };
    let bytes = pack(words, entries, &opts).unwrap();
    assert!(Resources::from_bytes(&bytes).is_ok());
    assert!(Resources::from_bytes(&bytes[..bytes.len() - 1]).is_err());
    let mut flipped = bytes.clone();
    let last = flipped.len() - 20;
    flipped[last] ^= 0x55;
    assert!(Resources::from_bytes(&flipped).is_err());
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(Resources::from_bytes(&extra).is_err());
    assert!(Resources::from_bytes(b"USNMQE\x00\x02").is_err());
}

#[test]
fn counts_are_the_studys() {
    let g = golden();
    let (mut entries, vocab) = counts(g["train"].as_str().unwrap(), 5);
    assert_eq!(vocab as u64, g["vocab"].as_u64().unwrap());
    let mut want: Vec<Entry> = g["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| Entry {
            key: e[0].as_str().unwrap().to_owned(),
            count: e[1].as_u64().unwrap() as u32,
            ctx: e[2].as_u64().unwrap() as u32,
            types: e[3].as_u64().unwrap() as u32,
        })
        .collect();
    entries.sort_by(|a, b| a.key.cmp(&b.key));
    want.sort_by(|a, b| a.key.cmp(&b.key));
    assert_eq!(entries, want);
}
