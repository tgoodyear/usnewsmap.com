//! The per-language resource file: the lexicon and the character model,
//! with the notice of what they were made from.
//!
//! ```text
//! USNMQE\0\x01                  magic and format version
//! u32 LE, notice                licences and sources, plain UTF-8 text
//! u32 LE, header                JSON (`Header`)
//! lexicon                       an `fst` set of folded words
//! model                         an `fst` map from n-gram to packed counts
//! ```
//!
//! A file is immutable and named by its sha256 wherever it is stored; the
//! job that ships it carries the checksum, and loading checks the two `fst`
//! checksums too.
use anyhow::{bail, ensure, Context as _};
use serde::{Deserialize, Serialize};

pub const MAGIC: &[u8; 8] = b"USNMQE\x00\x01";
/// The largest resource file a host accepts.
pub const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_NOTICE: usize = 256 * 1024;
const MAX_HEADER: usize = 64 * 1024;

/// Bits of the packed model value: the n-gram's count, its continuations'
/// total and how many distinct continuations it has.
const COUNT_BITS: u32 = 22;
const CTX_BITS: u32 = 22;
const TYPES_BITS: u32 = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    /// The measure the file serves (`super::VERSION`).
    pub measure: String,
    /// ISO 639-3.
    pub language: String,
    /// The character model's order and vocabulary size (characters seen in
    /// training plus one).
    pub order: u32,
    pub vocab: u32,
    pub lexicon_bytes: u64,
    pub model_bytes: u64,
    pub lexicon_words: u64,
    pub model_entries: u64,
    /// N-grams of this order or longer seen fewer than `prune_min_count`
    /// times were dropped (1: none).
    pub prune_min_count: u32,
    pub prune_from_order: u32,
    /// Whether the lexicon holds Hunspell forms (false: wordfreq only).
    pub hunspell: bool,
    /// SPDX expression of the file's licence.
    pub licence: String,
}

/// One entry of the model: an n-gram (or history), as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub count: u32,
    pub ctx: u32,
    pub types: u32,
}

pub(crate) fn pack_value(count: u32, ctx: u32, types: u32) -> anyhow::Result<u64> {
    ensure!(
        count < 1 << COUNT_BITS && ctx < 1 << CTX_BITS && types < 1 << TYPES_BITS,
        "a model count is too large to pack"
    );
    Ok((u64::from(count) << (CTX_BITS + TYPES_BITS))
        | (u64::from(ctx) << TYPES_BITS)
        | u64::from(types))
}

pub(crate) fn unpack_value(v: u64) -> (u32, u32, u32) {
    let mask = |bits: u32| (1u64 << bits) - 1;
    (
        ((v >> (CTX_BITS + TYPES_BITS)) & mask(COUNT_BITS)) as u32,
        ((v >> TYPES_BITS) & mask(CTX_BITS)) as u32,
        (v & mask(TYPES_BITS)) as u32,
    )
}

/// What `pack` needs besides the words and the model.
pub struct PackOptions<'a> {
    pub language: &'a str,
    pub order: u32,
    pub vocab: u32,
    pub prune_min_count: u32,
    pub prune_from_order: u32,
    pub hunspell: bool,
    pub licence: &'a str,
    pub notice: &'a str,
}

/// Whether pruning drops an entry. Pure histories (seen only as a context,
/// such as the padding at the start of the text) are never dropped.
pub fn pruned(e: &Entry, min_count: u32, from_order: u32) -> bool {
    e.count > 0 && e.count < min_count && e.key.chars().count() as u32 >= from_order
}

/// Writes a resource file. `words` need not be sorted or unique; `entries`
/// are pruned here.
pub fn pack(
    words: impl IntoIterator<Item = String>,
    entries: impl IntoIterator<Item = Entry>,
    o: &PackOptions,
) -> anyhow::Result<Vec<u8>> {
    let mut words: Vec<String> = words.into_iter().collect();
    words.sort_unstable();
    words.dedup();
    let lexicon = fst::Set::from_iter(words.iter()).context("building the lexicon")?;
    let mut kept: Vec<(String, u64)> = Vec::new();
    for e in entries {
        if !pruned(&e, o.prune_min_count, o.prune_from_order) {
            kept.push((e.key.clone(), pack_value(e.count, e.ctx, e.types)?));
        }
    }
    kept.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    kept.dedup_by(|a, b| a.0 == b.0);
    let model = fst::Map::from_iter(kept.iter().map(|(k, v)| (k.as_bytes(), *v)))
        .context("building the character model")?;
    let lexicon = lexicon.into_fst().into_inner();
    let model = model.into_fst().into_inner();
    let header = Header {
        measure: super::VERSION.to_owned(),
        language: o.language.to_owned(),
        order: o.order,
        vocab: o.vocab,
        lexicon_bytes: lexicon.len() as u64,
        model_bytes: model.len() as u64,
        lexicon_words: words.len() as u64,
        model_entries: kept.len() as u64,
        prune_min_count: o.prune_min_count,
        prune_from_order: o.prune_from_order,
        hunspell: o.hunspell,
        licence: o.licence.to_owned(),
    };
    let header = serde_json::to_vec(&header)?;
    ensure!(o.notice.len() <= MAX_NOTICE, "the notice is too long");
    let mut out =
        Vec::with_capacity(16 + o.notice.len() + header.len() + lexicon.len() + model.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(o.notice.len() as u32).to_le_bytes());
    out.extend_from_slice(o.notice.as_bytes());
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&lexicon);
    out.extend_from_slice(&model);
    ensure!(
        out.len() <= MAX_BYTES,
        "the resource file is over {MAX_BYTES} bytes"
    );
    Ok(out)
}

/// A resource file split into its parts, checked.
pub struct Parts {
    pub notice: String,
    pub header: Header,
    pub lexicon: fst::Set<Vec<u8>>,
    pub model: fst::Map<Vec<u8>>,
}

fn take<'a>(b: &mut &'a [u8], n: usize, what: &str) -> anyhow::Result<&'a [u8]> {
    ensure!(b.len() >= n, "the resource file ends inside its {what}");
    let (head, rest) = b.split_at(n);
    *b = rest;
    Ok(head)
}

fn take_u32(b: &mut &[u8], what: &str) -> anyhow::Result<usize> {
    let s = take(b, 4, what)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
}

pub fn unpack(bytes: &[u8]) -> anyhow::Result<Parts> {
    ensure!(bytes.len() <= MAX_BYTES, "the resource file is too large");
    let mut b = bytes;
    if take(&mut b, MAGIC.len(), "magic")? != MAGIC {
        bail!("not a quality-measure resource file of format 1");
    }
    let n = take_u32(&mut b, "notice length")?;
    ensure!(n <= MAX_NOTICE, "the notice is too long");
    let notice = String::from_utf8(take(&mut b, n, "notice")?.to_vec()).context("notice")?;
    let n = take_u32(&mut b, "header length")?;
    ensure!(n <= MAX_HEADER, "the header is too long");
    let header: Header = serde_json::from_slice(take(&mut b, n, "header")?).context("header")?;
    ensure!(
        header.measure == super::VERSION,
        "the resource file is for {}, not {}",
        header.measure,
        super::VERSION
    );
    ensure!(header.order >= 1 && header.order <= 8, "bad model order");
    ensure!(header.vocab >= 1, "bad model vocabulary");
    let lex = take(&mut b, usize::try_from(header.lexicon_bytes)?, "lexicon")?;
    let model = take(&mut b, usize::try_from(header.model_bytes)?, "model")?;
    ensure!(b.is_empty(), "the resource file has trailing bytes");
    let lexicon = fst::Set::new(lex.to_vec()).context("lexicon")?;
    lexicon.as_fst().verify().context("lexicon checksum")?;
    let model = fst::Map::new(model.to_vec()).context("model")?;
    model.as_fst().verify().context("model checksum")?;
    ensure!(
        lexicon.len() as u64 == header.lexicon_words && model.len() as u64 == header.model_entries,
        "the resource file's counts don't match its header"
    );
    Ok(Parts {
        notice,
        header,
        lexicon,
        model,
    })
}
