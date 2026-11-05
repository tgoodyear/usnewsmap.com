//! `loc-text` against a stand-in loc.gov. The item JSON and text service answers are
//! synthetic, shaped as `ocr/ja-ocr/damage_parity.py` reads loc.gov's: a page's files list a
//! `text/plain` file whose `fulltext_service` answers `{"<ALTO path>": {"full_text": …}}`.

use serde_json::json;

use super::*;
use crate::fetcher::Policy;
use crate::loc::global::{GlobalPacer, State};
use crate::standin::{Scripted, StandIn};

const LCCN: &str = "sn90052158";
const OTHER: &str = "sn82014200";
const BATCH: &str = "gu_allmanbrothers_ver01";
/// What LoC's datasets table gives as the archive's checksum, which no record may carry.
const ARCHIVE_SHA256: &str = "5eed5eed5eed5eed5eed5eed5eed5eed5eed5eed5eed5eed5eed5eed5eed5eed";

fn sha(b: &[u8]) -> String {
    usnm_core::digest::hex(&Sha256::digest(b))
}

fn policy(name: &str, host: &str) -> Policy {
    let mut p = Policy::parse(
        name,
        &format!(
            r#"
hosts = ["{host}"]
label = "{name}"
requests_per_minute = 60000
slow_requests_per_minute = 6000
[push_back]
default_hold_secs = 1
html = true
max_hold_secs = 3600
statuses = [403, 429]
"#
        ),
    )
    .unwrap();
    p.allow_plain_http = true;
    p
}

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn key(day: &str, seq: u16) -> PageKey {
    PageKey::new(LCCN, date(day), 1, seq).unwrap()
}

fn no_stop() -> watch::Receiver<bool> {
    let (tx, rx) = watch::channel(false);
    // The fetcher reads a dropped sender as a stop.
    std::mem::forget(tx);
    rx
}

struct Rig {
    dir: tempfile::TempDir,
    s: StandIn,
}

impl Rig {
    async fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            s: StandIn::start().await,
        }
    }

    fn out(&self) -> PathBuf {
        self.dir.path().join("out")
    }

    fn pacer(&self) -> PathBuf {
        self.dir.path().join("pacer.json")
    }

    fn access(&self, policies: Vec<Policy>) -> LocAccess {
        LocAccess {
            global_pacer: self.pacer(),
            policies: Policies::new(policies).unwrap(),
            gap: Some(Duration::ZERO),
        }
    }

    fn run_opts(&self) -> RunOptions {
        RunOptions {
            out: self.out(),
            api_root: self.s.root.clone(),
            loc: self.access(vec![policy(LOC, "127.0.0.1")]),
            max_hours: 1.0,
            limit: None,
        }
    }

    fn item_path(day: &str) -> String {
        format!("/item/{LCCN}/{day}/ed-1/")
    }

    /// The stand-in matches paths only, so each page's text URL has its own path.
    fn text_path(day: &str, seq: u16) -> String {
        format!("/text-services/{LCCN}/{day}/ed-1/seq-{seq}")
    }

    fn text_body(day: &str, seq: u16) -> Vec<u8> {
        let alto = format!(
            "/service/ndnp/gu/batch_{BATCH}/data/{LCCN}/00000000001/{}01/{seq:04}.xml",
            day.replace('-', "")
        );
        json!({ alto: {"full_text": format!("THE WIREGRASS {day} page {seq}\n"), "height": 9000, "width": 6000} })
            .to_string()
            .into_bytes()
    }

    /// An issue whose item JSON lists a page for each of `texts`, with that text URL, and each
    /// page's answer at its own path. Returns the item JSON.
    fn issue_with(&self, day: &str, texts: &[String]) -> Vec<u8> {
        let files: Vec<serde_json::Value> = texts
            .iter()
            .enumerate()
            .map(|(i, url)| {
                json!([
                    {"mimetype": "image/jp2", "url": format!("https://tile.loc.gov/storage-services/service/ndnp/gu/batch_{BATCH}/data/{LCCN}/00000000001/{}01/{:04}.jp2", day.replace('-', ""), i + 1)},
                    {"mimetype": "text/plain", "fulltext_service": url},
                ])
            })
            .collect();
        let item = json!({"item": {"id": format!("http://www.loc.gov/item/{LCCN}/{day}/ed-1/")}, "resources": [{"files": files}]})
            .to_string()
            .into_bytes();
        self.s.put(&Self::item_path(day), item.clone());
        for seq in 1..=u16::try_from(texts.len()).unwrap() {
            self.s
                .put(&Self::text_path(day, seq), Self::text_body(day, seq));
        }
        item
    }

    fn issue(&self, day: &str, pages: u16) -> Vec<u8> {
        let texts: Vec<String> = (1..=pages)
            .map(|seq| format!("{}{}", self.s.root, Self::text_path(day, seq)))
            .collect();
        self.issue_with(day, &texts)
    }

    fn queue(&self, keys: &[PageKey]) {
        std::fs::create_dir_all(self.out()).unwrap();
        let q = Queue {
            batch: BATCH.into(),
            pages: keys.iter().map(PageKey::doc_id).collect(),
        };
        std::fs::write(self.out().join(QUEUE), serde_json::to_vec(&q).unwrap()).unwrap();
    }

    fn record(&self, k: &PageKey) -> PageText {
        serde_json::from_slice(&std::fs::read(record_path(&self.out(), &k.doc_id())).unwrap())
            .unwrap()
    }

    fn status(&self) -> RunStatus {
        serde_json::from_slice(&std::fs::read(self.out().join(STATUS)).unwrap()).unwrap()
    }

    fn pacer_state(&self) -> State {
        GlobalPacer::new(self.pacer()).state().unwrap()
    }
}

/// A bag manifest: for each (lccn, issue directory, pages), the METS and a JP2 a page, after
/// a tag file.
fn manifest(issues: &[(&str, &str, usize)]) -> String {
    let md5 = |s: &str| batch::md5_hex(s.as_bytes());
    let mut out = format!("{}  bagit.txt\n", md5("bagit"));
    for (lccn, dir, pages) in issues {
        let at = format!("data/{lccn}/00000000001/{dir}");
        out.push_str(&format!("{}  {at}/{dir}.xml\n", md5(&at)));
        for frame in 1..=*pages {
            out.push_str(&format!(
                "{}  {at}/{frame:04}.jp2\n",
                md5(&format!("{at}{frame}"))
            ));
        }
    }
    out
}

struct Batch {
    bag: PathBuf,
    counts: PathBuf,
    datasets: PathBuf,
}

/// The Wiregrass on three days, one of them curated, and another title of the batch on two,
/// both curated, one with fewer pages than the bag lists.
fn batch_files(dir: &Path) -> Batch {
    let bag = dir.join("manifest-md5.txt");
    std::fs::write(
        &bag,
        manifest(&[
            (LCCN, "1902032701", 3),
            (LCCN, "1902040301", 2),
            (LCCN, "1902041701", 2),
            (OTHER, "1902032701", 4),
            (OTHER, "1902041001", 2),
        ]),
    )
    .unwrap();
    let day = |d: &str| day_number(date(d));
    let counts: BTreeMap<String, BTreeMap<u32, u32>> = BTreeMap::from([
        (
            OTHER.to_owned(),
            BTreeMap::from([(day("1902-03-27"), 4), (day("1902-04-10"), 1)]),
        ),
        (LCCN.to_owned(), BTreeMap::from([(day("1902-04-17"), 2)])),
    ]);
    let counts_path = dir.join("counts.json");
    std::fs::write(&counts_path, serde_json::to_vec(&counts).unwrap()).unwrap();
    let datasets = dir.join("datasets.json");
    std::fs::write(
        &datasets,
        json!({"datasets": [{
            "archive_name": format!("{BATCH}.tar.bz2"), "batch": BATCH,
            "url": format!("https://example.invalid/{BATCH}.tar.bz2"),
            "size": 1, "sha256": ARCHIVE_SHA256, "page_count": 13,
        }]})
        .to_string(),
    )
    .unwrap();
    Batch {
        bag,
        counts: counts_path,
        datasets,
    }
}

fn enqueue_opts(out: PathBuf, b: &Batch, lccn: Option<&str>, dry_run: bool) -> EnqueueOptions {
    EnqueueOptions {
        out,
        batch: BATCH.into(),
        lccn: lccn.map(str::to_owned),
        bag_manifest: b.bag.clone(),
        counts: b.counts.clone(),
        datasets: Some(b.datasets.clone()),
        dry_run,
        fetch_manifest: None,
    }
}

#[tokio::test]
async fn enqueue_plans_the_bag_pages_curation_lacks_once() {
    let dir = tempfile::tempdir().unwrap();
    let b = batch_files(dir.path());
    let out = dir.path().join("out");

    let plan = enqueue(enqueue_opts(out.clone(), &b, Some(LCCN), true), no_stop())
        .await
        .unwrap();
    assert!(!out.exists(), "a dry run writes nothing");
    assert_eq!(
        plan,
        Plan {
            batch: BATCH.into(),
            lccn: Some(LCCN.into()),
            dry_run: true,
            bag_manifest_sha256: sha(&std::fs::read(&b.bag).unwrap()),
            listed_pages: Some(13),
            held_pages: 7,
            missing_pages: Some(6),
            bag_issues: 5,
            bag_pages: 13,
            unreadable_issues: vec![],
            title_issues: 3,
            title_pages: 7,
            held_title_pages: 2,
            mismatched_days: vec![],
            planned: 5,
            first_date: Some(date("1902-03-27")),
            last_date: Some(date("1902-04-03")),
            already_queued: 0,
            already_fetched: 0,
            added: 5,
            queued: 5,
        }
    );

    let real = enqueue(enqueue_opts(out.clone(), &b, Some(LCCN), false), no_stop())
        .await
        .unwrap();
    assert_eq!((real.added, real.queued, real.dry_run), (5, 5, false));
    let q: Queue = serde_json::from_slice(&std::fs::read(out.join(QUEUE)).unwrap()).unwrap();
    let want: BTreeSet<String> = [
        key("1902-03-27", 1),
        key("1902-03-27", 2),
        key("1902-03-27", 3),
        key("1902-04-03", 1),
        key("1902-04-03", 2),
    ]
    .iter()
    .map(PageKey::doc_id)
    .collect();
    assert_eq!(q.pages, want);

    // Again, for every title: nothing is added twice, and the other title's days are curated,
    // one with a page count the bag disagrees with.
    let again = enqueue(enqueue_opts(out.clone(), &b, None, false), no_stop())
        .await
        .unwrap();
    assert_eq!(
        (again.planned, again.added, again.already_queued),
        (5, 0, 5)
    );
    assert_eq!(again.held_title_pages, 8);
    assert_eq!(
        again.mismatched_days,
        ["sn82014200 1902-04-10: held 1, bag 2"]
    );
    let q2: Queue = serde_json::from_slice(&std::fs::read(out.join(QUEUE)).unwrap()).unwrap();
    assert_eq!(q2, q);

    let e = enqueue(enqueue_opts(out, &b, Some("sn99999999"), true), no_stop())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("no issues of sn99999999"), "{e}");
}

/// A title curation holds whole still gets its (empty) queue, so its run is done at once.
#[tokio::test]
async fn a_title_missing_nothing_queues_nothing_and_its_run_is_done() {
    let r = Rig::new().await;
    let b = batch_files(r.dir.path());
    let plan = enqueue(enqueue_opts(r.out(), &b, Some(OTHER), false), no_stop())
        .await
        .unwrap();
    assert_eq!((plan.planned, plan.added, plan.queued), (0, 0, 0));
    let s = run(r.run_opts(), no_stop()).await.unwrap();
    assert_eq!((s.state.as_str(), s.queued, s.fetched), ("done", 0, 0));
    assert_eq!(r.s.hits_under("/"), 0);
}

/// Through the queue to the records: each names its responses' checksums, keeps the responses
/// as they came, and carries nothing of the bulk archive's.
#[tokio::test]
async fn pages_are_kept_with_each_responses_checksum_and_no_archive_checksum() {
    let r = Rig::new().await;
    let b = batch_files(r.dir.path());
    let item = r.issue("1902-03-27", 3);
    r.issue("1902-04-03", 2);
    enqueue(enqueue_opts(r.out(), &b, Some(LCCN), false), no_stop())
        .await
        .unwrap();

    let s = run(r.run_opts(), no_stop()).await.unwrap();
    assert_eq!(
        (
            s.state.as_str(),
            s.fetched,
            s.done,
            s.queued,
            s.items_fetched
        ),
        ("done", 5, 5, 5, 2)
    );
    assert!(s.failed.is_empty(), "{:?}", s.failed);
    assert_eq!(r.status(), s);
    assert_eq!(r.pacer_state().sent, 7, "every request in the budget");

    let k = key("1902-03-27", 2);
    let body = Rig::text_body("1902-03-27", 2);
    let rec = r.record(&k);
    assert_eq!(rec.doc_id, "sn90052158_1902-03-27_ed-1_seq-2");
    assert_eq!(rec.page_key, "sn90052158/1902-03-27/ed-1/seq-2");
    assert_eq!(
        (rec.batch.as_str(), rec.batch_version),
        ("gu_allmanbrothers", 1)
    );
    assert_eq!(rec.ocr_source, "loc-api-fulltext");
    assert_eq!(rec.text, "THE WIREGRASS 1902-03-27 page 2\n");
    assert_eq!(rec.text_sha256, sha(rec.text.as_bytes()));
    assert_eq!(rec.text_chars, 32);
    assert!(
        rec.alto_path.ends_with("/1902032701/0002.xml"),
        "{}",
        rec.alto_path
    );
    assert_eq!(
        rec.text_url,
        format!("{}{}", r.s.root, Rig::text_path("1902-03-27", 2))
    );
    assert_eq!(rec.response_sha256, sha(&body));
    assert_eq!(rec.response_bytes, body.len());
    assert_eq!(
        std::fs::read(
            r.out()
                .join("responses")
                .join(format!("{}.json", k.doc_id()))
        )
        .unwrap(),
        body
    );
    assert_eq!(
        rec.item_url,
        format!("{}/item/{LCCN}/1902-03-27/ed-1/?fo=json", r.s.root)
    );
    assert_eq!(rec.item_sha256, sha(&item));
    assert_eq!(
        std::fs::read(r.out().join("items/sn90052158_1902-03-27_ed-1.json")).unwrap(),
        item
    );
    let mut files = vec![r.out()];
    while let Some(p) = files.pop() {
        if p.is_dir() {
            files.extend(std::fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
        } else {
            assert!(!p.to_string_lossy().ends_with(".tmp"), "{}", p.display());
            let text = String::from_utf8_lossy(&std::fs::read(&p).unwrap()).into_owned();
            assert!(!text.contains(ARCHIVE_SHA256), "{}", p.display());
            assert!(!text.contains("archive"), "{}", p.display());
        }
    }
}

#[tokio::test]
async fn a_rerun_skips_pages_done_and_kept_items() {
    let r = Rig::new().await;
    r.issue("1902-03-27", 3);
    r.queue(&(1..=3).map(|n| key("1902-03-27", n)).collect::<Vec<_>>());

    let mut o = r.run_opts();
    o.limit = Some(1);
    let first = run(o, no_stop()).await.unwrap();
    assert_eq!(
        (first.state.as_str(), first.fetched, first.done),
        ("incomplete", 1, 1)
    );

    let second = run(r.run_opts(), no_stop()).await.unwrap();
    assert_eq!(
        (
            second.state.as_str(),
            second.fetched,
            second.done,
            second.items_fetched
        ),
        ("done", 2, 3, 0)
    );
    assert_eq!(r.s.hits(&Rig::item_path("1902-03-27")), 1);
    for n in 1..=3 {
        assert_eq!(r.s.hits(&Rig::text_path("1902-03-27", n)), 1, "seq {n}");
    }
    let third = run(r.run_opts(), no_stop()).await.unwrap();
    assert_eq!((third.state.as_str(), third.fetched), ("done", 0));
    assert_eq!(r.pacer_state().sent, 4);
}

#[tokio::test]
async fn a_run_needs_a_number_of_hours() {
    let r = Rig::new().await;
    r.issue("1902-03-27", 1);
    r.queue(&[key("1902-03-27", 1)]);
    for hours in [f64::NAN, f64::INFINITY] {
        let mut o = r.run_opts();
        o.max_hours = hours;
        let e = run(o, no_stop()).await.unwrap_err().to_string();
        assert!(e.contains("isn't a number of hours"), "{e}");
    }
    assert_eq!(r.s.hits_under("/"), 0);
}

#[tokio::test]
async fn a_push_back_slows_the_budget_and_ends_the_run() {
    let r = Rig::new().await;
    r.issue("1902-03-27", 3);
    r.issue("1902-04-03", 2);
    r.queue(&[
        key("1902-03-27", 1),
        key("1902-03-27", 2),
        key("1902-03-27", 3),
        key("1902-04-03", 1),
    ]);
    r.s.script(
        &Rig::text_path("1902-03-27", 2),
        Scripted::RetryAfter(429, 120),
    );
    let before = crate::loc::global::epoch_ms_in(Duration::from_secs(100));
    let e = run(r.run_opts(), no_stop()).await.unwrap_err().to_string();
    assert!(e.contains("HTTP 429") && e.contains("slow rate"), "{e}");
    let st = r.pacer_state();
    assert_eq!(st.slowed.unwrap().why, "LoC text: HTTP 429");
    assert!(st.hold_until_ms >= before, "Retry-After holds the budget");
    assert_eq!(r.status().state, "blocked");
    assert_eq!(r.s.hits(&Rig::text_path("1902-03-27", 3)), 0);
    assert_eq!(r.s.hits(&Rig::item_path("1902-04-03")), 0);
    assert!(record_path(&r.out(), &key("1902-03-27", 1).doc_id()).exists());
    assert!(!record_path(&r.out(), &key("1902-03-27", 2).doc_id()).exists());

    // Slowed, the next run asks nothing.
    let hits = r.s.hits_under("/");
    let e = run(r.run_opts(), no_stop()).await.unwrap_err().to_string();
    assert!(
        e.contains("slowed") && e.contains("LoC text: HTTP 429"),
        "{e}"
    );
    assert_eq!(r.s.hits_under("/"), hits);
    assert_eq!(r.status().state, "slowed");
}

/// Only LoC's hosts are asked for a page's text: a host another policy covers would be
/// outside LoC's budget, and one no policy covers is refused. A page the item JSON gives no
/// text and an answer that isn't one page's text fail that page alone.
#[tokio::test]
async fn text_off_locs_hosts_is_never_asked_and_a_bad_page_fails_alone() {
    let r = Rig::new().await;
    let mirror_root = r.s.root.replace("127.0.0.1", "localhost");
    let day = "1902-03-27";
    r.issue_with(
        day,
        &[
            format!("{mirror_root}{}", Rig::text_path(day, 1)),
            format!("https://example.invalid{}", Rig::text_path(day, 2)),
            format!("{}{}", r.s.root, Rig::text_path(day, 3)),
            format!("{}{}", r.s.root, Rig::text_path(day, 4)),
        ],
    );
    r.s.put(
        &Rig::text_path(day, 4),
        br#"{"/a.xml": {"full_text": "a"}, "/b.xml": {"full_text": "b"}}"#.to_vec(),
    );
    r.queue(&(1..=5).map(|n| key(day, n)).collect::<Vec<_>>());
    let mut o = r.run_opts();
    o.loc = r.access(vec![
        policy(LOC, "127.0.0.1"),
        policy("mirror", "localhost"),
    ]);

    let s = run(o, no_stop()).await.unwrap();
    assert_eq!((s.state.as_str(), s.fetched, s.done), ("incomplete", 1, 1));
    assert!(record_path(&r.out(), &key(day, 3).doc_id()).exists());
    let failed = s.failed.join("\n");
    assert_eq!(s.failed.len(), 4, "{failed}");
    assert!(s.failed[0].contains("mirror's host, not LoC's"), "{failed}");
    assert!(
        s.failed[1].contains("no policy covers example.invalid"),
        "{failed}"
    );
    assert!(s.failed[2].contains("2 entries, not one"), "{failed}");
    assert!(s.failed[3].contains("no text for seq 5"), "{failed}");
    assert_eq!(r.s.hits(&Rig::text_path(day, 1)), 0);
    assert_eq!(r.s.hits(&Rig::text_path(day, 4)), 1);
    assert_eq!(r.pacer_state().sent, 3, "the item and two pages");
    assert!(r.pacer_state().slowed.is_none());
}

/// A bag manifest that isn't on disk is one request inside the budget, kept for next time, and
/// never asked for on a dry run.
#[tokio::test]
async fn a_missing_manifest_is_fetched_once_but_never_on_a_dry_run() {
    let r = Rig::new().await;
    let b = batch_files(r.dir.path());
    let body = std::fs::read(&b.bag).unwrap();
    std::fs::remove_file(&b.bag).unwrap();
    let storage = format!("/storage-services/service/ndnp/gu/batch_{BATCH}/manifest-md5.txt");
    r.s.put(&storage, body.clone());
    r.s.redirect(&format!("/data/batches/{BATCH}/manifest-md5.txt"), &storage);
    let opts = |dry_run| EnqueueOptions {
        fetch_manifest: Some((r.s.batches_root(), r.access(vec![policy(LOC, "127.0.0.1")]))),
        ..enqueue_opts(r.out(), &b, Some(LCCN), dry_run)
    };

    let e = enqueue(opts(true), no_stop())
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("a dry run asks LoC nothing"), "{e}");
    assert_eq!(r.s.hits_under("/"), 0);
    assert!(!r.out().exists());

    let plan = enqueue(opts(false), no_stop()).await.unwrap();
    assert_eq!((plan.planned, plan.added), (5, 5));
    assert_eq!(plan.bag_manifest_sha256, sha(&body));
    assert_eq!(std::fs::read(&b.bag).unwrap(), body);
    assert_eq!(r.s.hits(&storage), 1);
    assert_eq!(r.pacer_state().sent, 2, "the redirect and the manifest");

    enqueue(opts(false), no_stop()).await.unwrap();
    assert_eq!(r.s.hits_under("/"), 2, "kept");
}

#[test]
fn a_text_answer_is_one_pages_text() {
    assert_eq!(
        parse_text(br#"{"/x/0001.xml": {"full_text": "A\nB", "height": 1, "width": 1}}"#),
        Ok(("/x/0001.xml".into(), "A\nB".into()))
    );
    for (body, why) in [
        (&b"<html>"[..], "isn't JSON"),
        (b"[]", "isn't a JSON object"),
        (b"{}", "0 entries"),
        (br#"{"/x.xml": {"height": 1}}"#, "no full_text"),
    ] {
        let e = parse_text(body).unwrap_err();
        assert!(e.contains(why), "{e}");
    }
}
