//! A batch's curated parts, downloaded ahead of the batch's turn
//! ([`Prefetch`], [`RestParts`]) and turned into main-index documents on
//! several threads ([`build_part_blocking`]).

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Context;
use futures::stream::{self, StreamExt, TryStreamExt};
use serde_json::Value;
use usnm_core::text::TextStatus;
use usnm_store::ObjectStore;

use super::{main_doc, page_doc, DocFormat, LocalReplaced, LocalRun};
use crate::american_stories;
use crate::catalog::Catalog;
use crate::curated::{read_part, CuratedRow};
use crate::dedup::{self, Plan};
use crate::ocr_ja;
use crate::ocr_local;
use crate::page_language as page_lang;
use crate::state::{JaLatinPage, RunBatch};

/// A batch's curated parts downloaded at once.
const CURATED_PREFETCH: usize = 8;

/// A batch's downloads, made ahead of its turn (`build_index`).
pub(super) struct Prefetched {
    pub(super) stories: Option<american_stories::Batch>,
    /// Every part in order, with its bytes if the prefetch got to it.
    pub(super) parts: Vec<(String, Option<bytes::Bytes>)>,
}

/// The task loading a batch's downloads, aborted when dropped (a release
/// that stops leaves nothing downloading).
pub(super) struct Prefetch(pub(super) tokio::task::JoinHandle<anyhow::Result<Prefetched>>);

impl Prefetch {
    pub(super) async fn join(mut self) -> anyhow::Result<Prefetched> {
        (&mut self.0).await.context("the batch prefetch task")?
    }
}

impl Drop for Prefetch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The curated parts a batch's prefetch holds at most, in bytes: past it,
/// the rest of the batch's parts download when the batch starts
/// ([`RestParts`]), so a large batch can't make two batches' parts sit in
/// memory at once.
const PREFETCH_PART_BYTES: usize = 512 * 1024 * 1024;

/// `b`'s American Stories text and curated parts, the parts a few at once,
/// up to [`PREFETCH_PART_BYTES`]; the parts not fetched yet are left as
/// `None`, in order.
pub(super) async fn prefetch_batch(
    curated: Arc<dyn ObjectStore>,
    stories: Option<Arc<american_stories::Source>>,
    b: RunBatch,
) -> anyhow::Result<Prefetched> {
    let stories = match stories {
        Some(source) => {
            let counts = dedup::load_counts(curated.as_ref(), &b).await?;
            Some(source.load(curated.as_ref(), &counts).await?)
        }
        None => None,
    };
    let store = curated.as_ref();
    let fetched = stream::iter(b.curated.parts.iter().cloned())
        .map(|path| async move {
            let bytes = fetch_part(store, &path).await?;
            Ok::<_, anyhow::Error>((path, bytes))
        })
        .buffered(CURATED_PREFETCH);
    let parts = held_parts(fetched, &b.curated.parts, PREFETCH_PART_BYTES).await?;
    Ok(Prefetched { stories, parts })
}

/// `paths` in order, each with its bytes from `fetched` (the same order)
/// until `limit` bytes are held, the rest `None`. Stopping drops the
/// downloads under way; those parts are fetched again in their turn.
async fn held_parts<S>(
    fetched: S,
    paths: &[String],
    limit: usize,
) -> anyhow::Result<Vec<(String, Option<bytes::Bytes>)>>
where
    S: futures::Stream<Item = anyhow::Result<(String, bytes::Bytes)>>,
{
    let mut fetched = std::pin::pin!(fetched);
    let mut parts = Vec::with_capacity(paths.len());
    let mut held = 0usize;
    while held < limit {
        match fetched.try_next().await? {
            Some((path, bytes)) => {
                held += bytes.len();
                parts.push((path, Some(bytes)));
            }
            None => break,
        }
    }
    for path in paths.iter().skip(parts.len()) {
        parts.push((path.clone(), None));
    }
    Ok(parts)
}

/// A batch's parts past the prefetch's limit ([`held_parts`]), downloaded
/// [`CURATED_PREFETCH`] at a time by a task of their own while the batch's
/// earlier parts are built, and handed over in order through a channel that
/// holds as many: at most twice [`CURATED_PREFETCH`] parts are in memory.
/// Dropping it stops the downloads.
pub(super) struct RestParts {
    rx: tokio::sync::mpsc::Receiver<anyhow::Result<(String, bytes::Bytes)>>,
    task: tokio::task::JoinHandle<()>,
}

impl RestParts {
    pub(super) fn start(curated: Arc<dyn ObjectStore>, paths: Vec<String>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(CURATED_PREFETCH);
        let task = tokio::spawn(async move {
            let store = curated.as_ref();
            let mut fetched = stream::iter(paths)
                .map(|path| async move {
                    let bytes = fetch_part(store, &path).await?;
                    Ok::<_, anyhow::Error>((path, bytes))
                })
                .buffered(CURATED_PREFETCH);
            while let Some(r) = fetched.next().await {
                let failed = r.is_err();
                if tx.send(r).await.is_err() || failed {
                    break;
                }
            }
        });
        Self { rx, task }
    }

    pub(super) async fn next(&mut self) -> anyhow::Result<(String, bytes::Bytes)> {
        self.rx
            .recv()
            .await
            .context("the remaining parts' download stopped")?
    }
}

impl Drop for RestParts {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One curated part.
async fn fetch_part(store: &dyn ObjectStore, path: &str) -> anyhow::Result<bytes::Bytes> {
    let bytes = store
        .get(path)
        .await?
        .with_context(|| format!("curated part `{path}` is missing"))?;
    Ok(bytes::Bytes::from(bytes))
}

/// What reading one curated part needs, all read-only.
pub(super) struct PartContext<'a> {
    pub(super) batch: &'a RunBatch,
    pub(super) dedup: &'a Plan,
    pub(super) texts: &'a HashMap<String, String>,
    pub(super) latin_curated: &'a HashMap<String, (&'a ocr_ja::JaPage, String)>,
    pub(super) latin_owned: &'a HashMap<String, String>,
    pub(super) catalog: &'a Catalog,
    pub(super) mine: &'a ocr_local::replace::Loaded,
    pub(super) local: Option<&'a LocalRun>,
    pub(super) format: DocFormat,
    /// Give each page its own languages (05 §5.5.8) as it is built.
    pub(super) page_language: bool,
}

/// One part's documents and what was recorded about them.
#[derive(Default)]
pub(super) struct PartDocs {
    pub(super) docs: Vec<Value>,
    /// Each document as the JSON line the writer gets, built beside it.
    pub(super) lines: Vec<Vec<u8>>,
    pub(super) replaced: Vec<(String, JaLatinPage)>,
    pub(super) tally: ocr_local::replace::Tally,
    pub(super) local_replaced: LocalReplaced,
    pub(super) with_as: u64,
    pub(super) only_as: u64,
    pub(super) without_locs: u64,
}

/// Threads that build a batch's documents (`USNM_RELEASE_THREADS`, default
/// the CPUs, at most 32).
fn part_threads() -> usize {
    std::env::var("USNM_RELEASE_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
        .clamp(1, 32)
}

/// Rows a thread builds at a time.
const ROWS_PER_CHUNK: usize = 256;

/// One part's documents ([`build_part`]): its rows decoded, then built
/// (with their page languages and JSON lines) on [`part_threads`] threads, a
/// chunk of rows each, in row order. On the release's multi-threaded
/// runtime, the runtime is told this thread blocks.
pub(super) fn build_part_blocking(
    ctx: &PartContext<'_>,
    path: &str,
    bytes: &bytes::Bytes,
) -> anyhow::Result<PartDocs> {
    let run = || build_part(ctx, bytes).with_context(|| path.to_owned());
    if tokio::runtime::Handle::try_current()
        .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
    {
        tokio::task::block_in_place(run)
    } else {
        run()
    }
}

/// One curated part's documents, as the release builds them.
fn build_part(ctx: &PartContext<'_>, bytes: &bytes::Bytes) -> anyhow::Result<PartDocs> {
    build_part_with(ctx, bytes, part_threads())
}

/// [`build_part`] on `threads` threads: the same documents, in the same
/// order, however many there are.
fn build_part_with(
    ctx: &PartContext<'_>,
    bytes: &bytes::Bytes,
    threads: usize,
) -> anyhow::Result<PartDocs> {
    let mut rows = Vec::new();
    read_part(bytes.clone(), true, |row| {
        rows.push(row);
        Ok(())
    })?;
    let chunks: Vec<&[CuratedRow]> = rows.chunks(ROWS_PER_CHUNK).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<anyhow::Result<PartDocs>>>> =
        chunks.iter().map(|_| std::sync::Mutex::new(None)).collect();
    let threads = threads.min(chunks.len()).max(1);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(chunk) = chunks.get(i) else {
                    break;
                };
                let mut out = PartDocs::default();
                let built = chunk
                    .iter()
                    .try_for_each(|row| build_row(ctx, row, &mut out))
                    .and_then(|()| {
                        for d in &mut out.docs {
                            if ctx.page_language {
                                page_lang::annotate_one(d);
                            }
                            out.lines.push(serde_json::to_vec(d)?);
                        }
                        Ok(())
                    });
                *slots[i].lock().expect("slot") = Some(built.map(|()| out));
            });
        }
    });
    let mut all = PartDocs::default();
    for slot in slots {
        let part = slot
            .into_inner()
            .expect("slot")
            .expect("every chunk built")?;
        all.docs.extend(part.docs);
        all.lines.extend(part.lines);
        all.replaced.extend(part.replaced);
        all.tally.merge(part.tally);
        all.local_replaced.extend(part.local_replaced);
        all.with_as += part.with_as;
        all.only_as += part.only_as;
        all.without_locs += part.without_locs;
    }
    Ok(all)
}

/// One row's document, if the version keeps it, and what is recorded about it.
fn build_row(ctx: &PartContext<'_>, row: &CuratedRow, out: &mut PartDocs) -> anyhow::Result<()> {
    let PartContext {
        batch: b,
        dedup,
        texts,
        latin_curated,
        latin_owned,
        catalog,
        mine,
        local,
        format,
        ..
    } = ctx;
    let (b, format) = (*b, *format);
    if !dedup.keeps(&row.key, &b.batch) {
        return Ok(());
    }
    let doc_id = row.key.doc_id();
    let text_as = if texts.is_empty() {
        None
    } else {
        texts.get(&doc_id).map(String::as_str)
    };
    // Our reading of the page, where it reads more words, on
    // the copy the version keeps (whichever batch's copy we read).
    let ours = latin_curated
        .get(&doc_id)
        .filter(|(_, t)| ocr_ja::better_than_locs(t, row.text.as_deref()));
    let ok = row.status == TextStatus::Ok;
    let title = catalog.title(&row.key.lccn).context("title")?;
    // Our own OCR of the page, where rule v1 prefers it to
    // LoC's text (Step 9). The Japanese overlay wins.
    let mut local_text: Option<&str> = None;
    if let Some(c) = mine.pages.get(&doc_id) {
        if let (Some(l), None) = (local, ours) {
            if l.ja.contains(&doc_id) {
                out.tally.keep("japanese_ocr");
            } else {
                let langs: Vec<&str> = title.languages.iter().map(String::as_str).collect();
                let (scores, _, verdict) =
                    ocr_local::rule::judge(&c.text, row.text.as_deref(), &langs);
                out.tally.weighed(c, &scores, verdict);
                if verdict.is_ok() {
                    local_text = Some(c.text.as_str());
                    out.local_replaced
                        .push((doc_id.clone(), c.engine(), b.batch.clone()));
                }
            }
        } else {
            out.tally.keep("japanese_ocr");
        }
    } else if mine.revoked.contains(&doc_id) {
        out.tally.keep("revoked");
    }
    if !ok && text_as.is_none() && ours.is_none() && local_text.is_none() {
        return Ok(());
    }
    // A copy without LoC's text of a page whose document is
    // ours (an earlier index's, or a page LoC's archive had
    // no text for in another batch): ours stays the page's.
    if !ok
        && latin_owned
            .get(&doc_id)
            .is_some_and(|ours| *ours != b.batch)
    {
        return Ok(());
    }
    out.with_as += u64::from(text_as.is_some());
    out.only_as += u64::from(!ok && ours.is_none() && local_text.is_none());
    out.without_locs += u64::from(!ok);
    let place = catalog.place_on(title, row.key.date).context("place")?;
    out.docs.push(match ours {
        Some((p, text)) => {
            out.replaced.push((
                doc_id,
                JaLatinPage {
                    // The document's batch, which hidden copies name.
                    batch: b.batch.clone(),
                    loc_text: p.loc_text.clone(),
                    ocr_source: p.ocr_source.clone(),
                    ocr_engine: p.ocr_engine.clone(),
                },
            ));
            main_doc(&row.key, &row.batch, text, title, place, text_as, format)
        }
        None => match local_text {
            Some(text) => main_doc(&row.key, &row.batch, text, title, place, text_as, format),
            None => page_doc(row, title, place, text_as, format),
        },
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashSet};

    use chrono::{DateTime, NaiveDate};
    use usnm_core::text::Analyzer;
    use usnm_core::text_layout::TextLayout;

    use super::*;
    use crate::catalog::{Place, Title};
    use crate::state::Curated;

    /// LoC's reading of an English page, garbled, and ours of it, clean
    /// (`ocr_local::rule`'s golden texts).
    const LOC_GARBLED: &str = "Tbe cify councll met on Tuesdav and votcd for tbe ncw scbool. \
        Tlie mayor said tbe railroad wouid bring more trade to tlie town. Cotton prices \
        rosc in tbe market and tbe farmers were glad. The weatber was fair aud tbe roads \
        were dry. A new cburch will be built on Main street ncxt spring by the people.";
    const OURS_CLEAN: &str = "The city council met on Tuesday and voted for the new school. \
        The mayor said the railroad would bring more trade to the town. Cotton prices \
        rose in the market and the farmers were glad. The weather was fair and the roads \
        were dry. A new church will be built on Main street next spring by the people.";
    /// LoC's reading of a page with few words in it, which our Latin
    /// reading beats.
    const NOISE: &str = "T1e c0unc1l m3t 0n Tu3sd4y 4nd v0t3d f0r th3 n3w sch00l; th3 m4y0r \
        s41d th3 r41lr04d w0u1d br1ng m0r3 tr4d3 t0 th3 t0wn 4nd c0tt0n pr1c3s r0s3.";
    const GERMAN: &str = "Die Stadt hat gestern einen neuen Bürgermeister gewählt, und die \
        Bauern sind mit dem Wetter und den Preisen auf dem Markt sehr zufrieden. Die neue \
        Kirche wird im Frühjahr gebaut, sagte der Pfarrer nach der Versammlung im Rathaus.";

    /// Our own OCR of a page.
    fn candidate(doc_id: &str, text: &str) -> ocr_local::replace::Candidate {
        ocr_local::replace::Candidate {
            row: ocr_local::pages::StoredRow {
                doc_id: doc_id.into(),
                lccn: "sn1".into(),
                batch: "batch_a".into(),
                ocr_source: "usnm-local".into(),
                ocr_engine: "tesseract/5 profile=fast".into(),
                node: "node1".into(),
                metric_version: usnm_core::damage::VERSION,
                ocred_at: 0,
                ours: Default::default(),
                loc: Default::default(),
            },
            text: text.into(),
        }
    }

    #[tokio::test]
    async fn the_remaining_parts_arrive_in_order_and_a_missing_one_stops_them() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn ObjectStore> = Arc::new(usnm_store::local::LocalStore::new(dir.path()));
        let paths: Vec<String> = (0..20)
            .map(|i| format!("pages/b/p{i:02}.parquet"))
            .collect();
        for (i, p) in paths.iter().enumerate() {
            store
                .put(p, vec![i as u8; 3], "application/octet-stream")
                .await
                .unwrap();
        }
        let mut rest = RestParts::start(Arc::clone(&store), paths.clone());
        for (i, p) in paths.iter().enumerate() {
            let (got, bytes) = rest.next().await.unwrap();
            assert_eq!((&got, bytes.as_ref()), (p, &[i as u8; 3][..]));
        }
        assert!(rest.next().await.is_err(), "nothing after the last part");
        let mut missing = paths[..3].to_vec();
        missing.insert(1, "pages/b/gone.parquet".into());
        let mut rest = RestParts::start(store, missing);
        assert_eq!(rest.next().await.unwrap().0, paths[0]);
        let err = rest.next().await.unwrap_err().to_string();
        assert!(err.contains("gone.parquet"), "{err}");
        assert!(
            rest.next().await.is_err(),
            "the downloads stop at the first failure"
        );
    }

    #[tokio::test]
    async fn a_prefetch_holds_parts_up_to_its_limit_and_leaves_the_rest_in_order() {
        let paths: Vec<String> = (0..5).map(|i| format!("p{i}")).collect();
        let fetched = |paths: Vec<String>| {
            stream::iter(
                paths
                    .into_iter()
                    .map(|p| Ok((p, bytes::Bytes::from(vec![0u8; 10])))),
            )
        };
        let parts = held_parts(fetched(paths.clone()), &paths, 25)
            .await
            .unwrap();
        let got: Vec<(&str, bool)> = parts
            .iter()
            .map(|(p, b)| (p.as_str(), b.is_some()))
            .collect();
        assert_eq!(
            got,
            [
                ("p0", true),
                ("p1", true),
                ("p2", true),
                ("p3", false),
                ("p4", false)
            ]
        );
        let all = held_parts(fetched(paths.clone()), &paths, usize::MAX)
            .await
            .unwrap();
        assert!(all.iter().all(|(_, b)| b.is_some()));
        assert_eq!(all.len(), 5);
    }

    #[test]
    fn parts_build_the_same_on_any_number_of_threads() {
        let catalog = Catalog::new(
            vec![Title {
                lccn: "sn1".into(),
                name: "sn1".into(),
                ordinal: 1,
                place_id: "P1".into(),
                state: "IL".into(),
                languages: vec!["eng".into(), "ger".into()],
                place_periods: vec![],
                extra: BTreeMap::new(),
            }],
            vec![Place {
                id: "P1".into(),
                ordinal: 1,
                name: "P1".into(),
                state: "IL".into(),
                lat: 41.0,
                lon: -87.0,
                precision: "city".into(),
                coordinates_from: None,
                variants: vec![],
            }],
        )
        .unwrap();
        let batch = RunBatch {
            batch: "batch_a".into(),
            curated: Curated {
                version: 1,
                parts: vec![],
                counts: String::new(),
                pages: 0,
                ok_pages: 0,
                lccns: vec![],
                source_sha256: String::new(),
                first: String::new(),
                last: String::new(),
            },
        };
        // 700 rows (three chunks of `ROWS_PER_CHUNK`), each tenth one kind.
        let at = DateTime::from_timestamp(1_790_000_000, 0).unwrap();
        let mut part = crate::curated::PartWriter::new();
        let mut dropped = Vec::new();
        let mut texts = HashMap::new();
        let mut ja_pages = Vec::new();
        let mut latin_owned = HashMap::new();
        let mut mine = ocr_local::replace::Loaded::default();
        let mut ja = HashSet::new();
        let mut kept = Vec::new();
        for i in 0..700u16 {
            let date = NaiveDate::from_ymd_opt(1896, 7, 1 + u32::from(i / 100)).unwrap();
            let key = usnm_core::ids::PageKey::new("sn1", date, 1, i % 100 + 1).unwrap();
            let id = key.doc_id();
            let raw = match i % 10 {
                1 | 2 => "a few words".to_owned(),
                9 if (i / 10) % 2 == 0 => "a few words".to_owned(),
                9 => format!("{GERMAN} {i}"),
                4 => format!("{NOISE} {i}"),
                5 => format!("{LOC_GARBLED} {i}"),
                _ => format!("{OURS_CLEAN} {i}"),
            };
            match i % 10 {
                // Another batch's copy is kept.
                0 => dropped.push((key.clone(), "batch_b")),
                // Only American Stories' text, or both.
                2 | 3 => {
                    texts.insert(id.clone(), format!("American Stories' page {i}"));
                }
                // Our Latin reading of the page (and some of our own OCR,
                // which it wins over).
                4 => {
                    ja_pages.push((
                        id.clone(),
                        ocr_ja::JaPage::for_test(key.clone(), "batch_a", "garbled", "x"),
                    ));
                    if (i / 10) % 3 == 0 {
                        mine.pages
                            .insert(id.clone(), candidate(&id, &format!("{OURS_CLEAN} {i}")));
                    }
                }
                // Our own OCR: cleaner, no better, or the overlay's page.
                5 | 7 => {
                    mine.pages
                        .insert(id.clone(), candidate(&id, &format!("{OURS_CLEAN} {i}")));
                    if i % 10 == 7 {
                        ja.insert(id.clone());
                    }
                }
                6 => {
                    mine.pages
                        .insert(id.clone(), candidate(&id, &format!("{LOC_GARBLED} {i}")));
                }
                8 => {
                    mine.revoked.insert(id.clone());
                }
                // American Stories' text of a page another batch's
                // document is ours.
                9 if (i / 10) % 2 == 0 => {
                    texts.insert(id.clone(), format!("American Stories' page {i}"));
                    latin_owned.insert(id.clone(), "batch_b".to_owned());
                }
                _ => {}
            }
            if !matches!(i % 10, 0 | 1) && !(i % 10 == 9 && (i / 10) % 2 == 0) {
                kept.push(id.clone());
            }
            let row = CuratedRow::from_ocr(key, &raw, "batch_a", 1, "ndnp-original", at);
            assert!(part.push(row).unwrap().is_none());
        }
        let bytes = bytes::Bytes::from(part.finish_part().unwrap().unwrap());
        let latin_curated: HashMap<String, (&ocr_ja::JaPage, String)> = ja_pages
            .iter()
            .map(|(id, p)| (id.clone(), (p, format!("{OURS_CLEAN} {id}"))))
            .collect();
        let dedup = Plan::keeping(dropped);
        let local = LocalRun {
            listing: Default::default(),
            revoked: vec![],
            ja,
            estimate: None,
        };
        let ctx = PartContext {
            batch: &batch,
            dedup: &dedup,
            texts: &texts,
            latin_curated: &latin_curated,
            latin_owned: &latin_owned,
            catalog: &catalog,
            mine: &mine,
            local: Some(&local),
            format: DocFormat {
                decade: true,
                analyzer: Analyzer::V2,
                text: TextLayout::Separate,
            },
            page_language: true,
        };

        let one = build_part_with(&ctx, &bytes, 1).unwrap();
        // Every path the fixture means to cover, so it can't stop quietly.
        assert_eq!(one.docs.len(), 525);
        assert_eq!((one.with_as, one.only_as, one.without_locs), (140, 70, 70));
        assert_eq!(one.replaced.len(), 70);
        assert_eq!(one.local_replaced.len(), 70);
        assert_eq!(one.tally.replaced, 70);
        assert_eq!(one.tally.kept.get("margin"), Some(&70), "{:?}", one.tally);
        assert_eq!(one.tally.kept.get("revoked"), Some(&70));
        assert_eq!(one.tally.kept.get("japanese_ocr"), Some(&(70 + 24)));
        let langs: BTreeSet<String> = one
            .docs
            .iter()
            .map(|d| d[usnm_core::page_language::SET_FIELD].to_string())
            .collect();
        assert_eq!(langs.len(), 3, "{langs:?}");
        assert_eq!(one.lines.len(), one.docs.len());
        for (doc, line) in one.docs.iter().zip(&one.lines) {
            assert_eq!(*line, serde_json::to_vec(doc).unwrap());
        }
        let ids: Vec<&str> = one
            .docs
            .iter()
            .map(|d| d["doc_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, kept, "documents in row order");

        for threads in [2, 3, 4, 8] {
            let many = build_part_with(&ctx, &bytes, threads).unwrap();
            assert_eq!(many.docs, one.docs, "{threads} threads");
            assert_eq!(many.lines, one.lines, "{threads} threads");
            assert_eq!(many.replaced, one.replaced, "{threads} threads");
            assert_eq!(many.local_replaced, one.local_replaced, "{threads} threads");
            assert_eq!(many.tally, one.tally, "{threads} threads");
            assert_eq!(
                (many.with_as, many.only_as, many.without_locs),
                (one.with_as, one.only_as, one.without_locs),
                "{threads} threads"
            );
        }
    }
}
