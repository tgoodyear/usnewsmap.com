//! Packs and applies the reference-free OCR quality measure's resource files
//! (`usnm_core::qe`).
//!
//! ```text
//! qe --command pack --build DIR --lexicon words.txt --notice FILE --licence SPDX
//!    [--prune-min-count 1 --prune-from-order 5] --out FILE
//! qe --command score --resources DIR --rows FILE [--text-field text] --out FILE
//! qe --command info --file FILE
//! ```
//!
//! `pack` reads one language's directory from `scripts/qe/build_resources.py`
//! (`meta.json`, `charlm.jsonl` and the named lexicon) and writes the file.
//! `score` reads JSON lines with `lang` and the text field and writes them
//! back with the measures added, using `<lang>.qe` under `--resources`; rows
//! of a language with no file get none.
use std::collections::BTreeMap;
use std::io::{BufRead as _, BufWriter, Write as _};
use std::path::{Path, PathBuf};
use usnm_core::qe;

fn flags(args: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let mut it = args.iter();
    while let Some(k) = it.next() {
        let k = k
            .strip_prefix("--")
            .ok_or_else(|| anyhow::anyhow!("expected a --flag, got {k:?}"))?;
        let v = it
            .next()
            .ok_or_else(|| anyhow::anyhow!("--{k} needs a value"))?;
        out.insert(k.to_owned(), v.clone());
    }
    Ok(out)
}

fn get<'a>(f: &'a BTreeMap<String, String>, k: &str) -> anyhow::Result<&'a str> {
    f.get(k)
        .map(String::as_str)
        .ok_or_else(|| anyhow::anyhow!("--{k} is required"))
}

fn pack(f: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let build = PathBuf::from(get(f, "build")?);
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(build.join("meta.json"))?)?;
    let lexicon = get(f, "lexicon")?;
    let words: Vec<String> = std::fs::read_to_string(build.join(lexicon))?
        .lines()
        .map(str::to_owned)
        .collect();
    let mut entries = Vec::new();
    for line in std::io::BufReader::new(std::fs::File::open(build.join("charlm.jsonl"))?).lines() {
        let (key, count, ctx, types): (String, u32, u32, u32) = serde_json::from_str(&line?)?;
        entries.push(qe::Entry {
            key,
            count,
            ctx,
            types,
        });
    }
    let notice = std::fs::read_to_string(get(f, "notice")?)?;
    let num = |k: &str, d: u32| -> anyhow::Result<u32> {
        Ok(f.get(k).map(|v| v.parse()).transpose()?.unwrap_or(d))
    };
    let lang = meta["lang"].as_str().unwrap_or_default().to_owned();
    let bytes = qe::pack(
        words,
        entries,
        &qe::PackOptions {
            language: &lang,
            order: meta["order"].as_u64().unwrap_or(5) as u32,
            vocab: meta["vocab"].as_u64().unwrap_or(0) as u32,
            prune_min_count: num("prune-min-count", 1)?,
            prune_from_order: num("prune-from-order", 5)?,
            hunspell: lexicon != "words-wordfreq.txt",
            licence: get(f, "licence")?,
            notice: &notice,
        },
    )?;
    let r = qe::Resources::from_bytes(&bytes)?;
    std::fs::write(get(f, "out")?, &bytes)?;
    let h = r.header();
    println!(
        "{}",
        serde_json::json!({
            "language": h.language, "bytes": bytes.len(), "sha256": usnm_core::sha256_hex(&bytes),
            "lexicon_bytes": h.lexicon_bytes, "model_bytes": h.model_bytes,
            "lexicon_words": h.lexicon_words, "model_entries": h.model_entries,
            "prune_min_count": h.prune_min_count, "prune_from_order": h.prune_from_order,
            "hunspell": h.hunspell,
        })
    );
    Ok(())
}

fn score(f: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let dir = Path::new(get(f, "resources")?);
    let field = f.get("text-field").map_or("text", String::as_str);
    let mut loaded: BTreeMap<String, Option<(qe::Resources, String)>> = BTreeMap::new();
    let mut out = BufWriter::new(std::fs::File::create(get(f, "out")?)?);
    for line in std::io::BufReader::new(std::fs::File::open(get(f, "rows")?)?).lines() {
        let mut row: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&line?)?;
        let lang = row
            .get("lang")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        let res = loaded.entry(lang.clone()).or_insert_with(|| {
            let bytes = std::fs::read(dir.join(format!("{lang}.qe"))).ok()?;
            let sha = usnm_core::sha256_hex(&bytes);
            Some((qe::Resources::from_bytes(&bytes).ok()?, sha))
        });
        if let Some((r, sha)) = res {
            let text = row.get(field).and_then(|v| v.as_str()).unwrap_or_default();
            let s = r.score(text);
            row.insert("qe".into(), serde_json::to_value(s)?);
            row.insert("qe_version".into(), qe::VERSION.into());
            row.insert("qe_resources".into(), sha.clone().into());
        }
        row.remove(field);
        row.remove("ref");
        serde_json::to_writer(&mut out, &row)?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(())
}

fn info(f: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let bytes = std::fs::read(get(f, "file")?)?;
    let r = qe::Resources::from_bytes(&bytes)?;
    println!("{}", serde_json::to_string_pretty(r.header())?);
    println!("sha256 {}\n\n{}", usnm_core::sha256_hex(&bytes), r.notice());
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let f = flags(&args)?;
    let cmd = get(&f, "command")?;
    match cmd {
        "pack" => pack(&f),
        "score" => score(&f),
        "info" => info(&f),
        _ => anyhow::bail!("unknown command {cmd}"),
    }
}
