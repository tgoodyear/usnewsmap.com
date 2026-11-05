//! GHN's source against a stand-in server, with tiny synthetic IIIF documents shaped like
//! GHN's own (no GHN content).

use std::path::PathBuf;
use std::time::Duration;

use chrono::NaiveDate;
use serde_json::{json, Value};
use tokio::sync::watch;

use super::catalog::{self, Decision};
use super::fetch::{self, Sidecar};
use super::iiif::{self, Hosts, IssueRef};
use crate::fetcher::{Policies, Policy, GHN};
use crate::standin::{Scripted, StandIn};

const WIREGRASS: &str = "sn90052158";
/// A title we have, one we lack, one with nothing up to 1930 and one GHN won't describe.
const OURS: &str = "sn11111111";
const LACK: &str = "sn22222222";
const LATE: &str = "sn33333333";
const BARE: &str = "sn44444444";

/// A JPEG's first bytes and a little more.
const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 4];

fn day(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn sha(b: &[u8]) -> String {
    super::sha256_hex(b)
}

fn no_stop() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}

fn issue(lccn: &str, date: &str, edition: u16) -> IssueRef {
    IssueRef {
        lccn: lccn.into(),
        date: day(date),
        edition,
    }
}

/// GHN's hosts as the stand-in: the site on `127.0.0.1`, images on `localhost`.
fn hosts(s: &StandIn) -> Hosts {
    let root = s.iiif_root().trim_end_matches("/iiif/").to_owned();
    Hosts {
        images: format!(
            "{}/iiif/2/newspapers/",
            root.replace("127.0.0.1", "localhost")
        ),
        site: root,
    }
}

/// The built-in `ghn` policy on the stand-in's hosts, at test speed.
fn ghn_policy() -> Policy {
    let mut p: Policy = Policies::builtin()
        .iter()
        .find(|p| p.name == GHN)
        .unwrap()
        .clone();
    p.hosts = vec!["127.0.0.1".into(), "localhost".into()];
    p.allow_plain_http = true;
    p.requests_per_minute = 60000;
    p.slow_requests_per_minute = Some(6000);
    p.min_gap_secs = 0;
    p.retry_backoff_secs = 0;
    p
}

fn policies() -> Policies {
    Policies::new(vec![ghn_policy()]).unwrap()
}

fn newspapers_doc(h: &Hosts, lccns: &[&str]) -> Value {
    let titles: Vec<Value> = lccns
        .iter()
        .map(|l| json!({"@id": h.title(l), "@type": "sc:Collection", "label": format!("Title {l}.")}))
        .collect();
    json!({"@id": h.newspapers(), "@type": "sc:Collection", "label": "Newspapers", "collections": titles})
}

fn title_doc(h: &Hosts, lccn: &str, issues: &[(&str, u16)]) -> Value {
    let manifests: Vec<Value> = issues
        .iter()
        .map(|(d, e)| json!({"@id": issue(lccn, d, *e).url(h), "@type": "sc:Manifest", "label": d}))
        .collect();
    json!({"@id": h.title(lccn), "@type": "sc:Collection", "label": format!("Title {lccn}."), "manifests": manifests})
}

/// A manifest whose canvases are `(seq, image identifier)`, in the order given.
fn issue_doc(h: &Hosts, i: &IssueRef, pages: &[(u16, &str)]) -> Value {
    let url = i.url(h);
    let base = url.trim_end_matches(".json");
    let canvases: Vec<Value> = pages
        .iter()
        .map(|(seq, ident)| {
            let service = format!("{}{ident}", h.images);
            json!({
                "@id": service, "@type": "sc:Canvas", "label": seq.to_string(),
                "width": 50, "height": 70,
                "images": [{
                    "@type": "oa:Annotation", "motivation": "sc:painting",
                    "resource": {"@id": service, "@type": "dctypes:Image", "service": {"@id": service}},
                    "rendering": [
                        {"@id": format!("{base}/seq-{seq}.pdf"), "format": "application/pdf"},
                        {"@id": format!("{base}/seq-{seq}/"), "format": "text/html"},
                    ],
                }],
            })
        })
        .collect();
    json!({"@id": url, "@type": "sc:Manifest", "label": "x", "sequences": [{"canvases": canvases}]})
}

fn path_of(url: &str) -> String {
    reqwest::Url::parse(url).unwrap().path().to_owned()
}

// ---- IIIF documents ----

#[test]
fn a_manifests_pages_take_ghns_numbers_and_ids() {
    let h = Hosts::ghn();
    let i = issue("sn89053815", "1876-10-15", 1);
    let v = issue_doc(&h, &i, &[(2, "b%2F0002.jp2"), (1, "b%2F0001.jp2")]);
    let pages = iiif::issue(&v, &i, &h).unwrap();
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].key.seq, 1, "its page link, not the canvas's place");
    assert_eq!(
        pages[0].id.to_string(),
        "ghn:sn89053815_1876-10-15_ed-1_seq-1"
    );
    assert_eq!(
        pages[0].image,
        "https://iiif-ha.galib.uga.edu/iiif/2/newspapers/b%2F0001.jp2/full/full/0/default.jpg"
    );
    assert_eq!((pages[0].width, pages[0].height), (Some(50), Some(70)));
    // Another issue's manifest isn't this one.
    assert!(iiif::issue(&v, &issue("sn89053815", "1876-10-17", 1), &h).is_err());
}

#[test]
fn a_manifest_is_refused_for_a_foreign_image_or_an_unnumbered_or_repeated_page() {
    let h = Hosts::ghn();
    let i = issue("sn89053815", "1876-10-15", 1);
    let refused = |v: Value| iiif::issue(&v, &i, &h).is_err();
    let mut v = issue_doc(&h, &i, &[(1, "a.jp2")]);
    v["sequences"][0]["canvases"][0]["images"][0]["resource"]["service"]["@id"] =
        "https://elsewhere.example/iiif/2/newspapers/a.jp2".into();
    assert!(refused(v));
    assert!(refused(issue_doc(&h, &i, &[(1, "a/../b.jp2")])));
    let mut v = issue_doc(&h, &i, &[(1, "a.jp2")]);
    v["sequences"][0]["canvases"][0]["images"][0]["rendering"] = json!([]);
    assert!(refused(v));
    assert!(refused(issue_doc(&h, &i, &[(1, "a.jp2"), (1, "b.jp2")])));
    let mut v = issue_doc(&h, &i, &[(1, "a.jp2")]);
    v["sequences"] = json!([]);
    assert!(refused(v));
}

#[test]
fn issue_urls_are_read_only_in_their_canonical_form() {
    let h = Hosts::ghn();
    let url = "https://gahistoricnewspapers.galileo.usg.edu/lccn/sn89053815/1876-10-15/ed-1.json";
    assert_eq!(h.issue_of(url), Some(issue("sn89053815", "1876-10-15", 1)));
    for bad in [
        "https://gahistoricnewspapers.galileo.usg.edu/lccn/sn89053815/1876-10-15/ed-01.json",
        "https://gahistoricnewspapers.galileo.usg.edu/lccn/sn89053815/1876-10-5/ed-1.json",
        "https://gahistoricnewspapers.galileo.usg.edu/lccn/sn89053815/1876-10-15/ed-0.json",
        "https://gahistoricnewspapers.galileo.usg.edu/lccn/SN89053815/1876-10-15/ed-1.json",
        "http://gahistoricnewspapers.galileo.usg.edu/lccn/sn89053815/1876-10-15/ed-1.json",
        "https://elsewhere.example/lccn/sn89053815/1876-10-15/ed-1.json",
    ] {
        assert_eq!(h.issue_of(bad), None, "{bad}");
    }
}

#[test]
fn title_and_newspapers_documents_are_checked() {
    let h = Hosts::ghn();
    let mut v = title_doc(&h, LACK, &[("1900-01-02", 1), ("1900-01-01", 1)]);
    let t = iiif::title(&v, LACK, &h).unwrap();
    assert_eq!(t.issues[0].date, day("1900-01-01"));
    assert!(
        iiif::title(&v, OURS, &h).is_err(),
        "its @id is another title's"
    );
    v["manifests"][0]["@id"] = issue(OURS, "1900-01-02", 1).url(&h).into();
    assert!(iiif::title(&v, LACK, &h).is_err(), "another title's issue");

    let v = newspapers_doc(&h, &[LACK, OURS]);
    let titles = iiif::newspapers(&v, &h).unwrap();
    assert_eq!(titles[0].lccn, OURS);
    assert!(iiif::newspapers(&newspapers_doc(&h, &[LACK, LACK]), &h).is_err());
    assert!(iiif::newspapers(&newspapers_doc(&h, &["../x"]), &h).is_err());
}

#[test]
fn only_noc_us_is_a_public_domain_claim() {
    assert!(super::is_noc_us(
        "http://rightsstatements.org/vocab/NoC-US/1.0/"
    ));
    assert!(super::is_noc_us(
        "https://rightsstatements.org/vocab/NoC-US/1.0"
    ));
    assert!(!super::is_noc_us(
        "http://rightsstatements.org/vocab/InC/1.0/"
    ));
    assert!(!super::is_noc_us(
        "http://rightsstatements.org/vocab/NoC-US/1.0/x"
    ));
    assert!(!super::is_noc_us("unchecked"));
}

// ---- the catalogue ----

struct Cat {
    dir: tempfile::TempDir,
}

impl Cat {
    /// A cache with every title listed and collections for all but [`BARE`].
    fn new(h: &Hosts) -> Self {
        let c = Cat {
            dir: tempfile::tempdir().unwrap(),
        };
        let lccns = [OURS, LACK, LATE, BARE, WIREGRASS];
        c.put("newspapers.json", &newspapers_doc(h, &lccns));
        c.put(
            &format!("lccn/{OURS}.json"),
            &title_doc(h, OURS, &[("1900-01-01", 1)]),
        );
        let lack = [("1929-05-01", 1), ("1930-12-31", 2), ("1931-01-01", 1)];
        c.put(&format!("lccn/{LACK}.json"), &title_doc(h, LACK, &lack));
        c.put(
            &format!("lccn/{LATE}.json"),
            &title_doc(h, LATE, &[("1931-01-01", 1)]),
        );
        c.put(
            &format!("lccn/{WIREGRASS}.json"),
            &title_doc(h, WIREGRASS, &[("1920-01-01", 1)]),
        );
        let ours = json!({"batches": [{"name": "batch_x", "curated": {"lccns": [OURS]}}]});
        std::fs::write(c.ours(), ours.to_string()).unwrap();
        c
    }

    fn cache(&self) -> PathBuf {
        self.dir.path().join("cache")
    }

    fn ours(&self) -> PathBuf {
        self.dir.path().join("batches.json")
    }

    fn out(&self) -> PathBuf {
        self.dir.path().join("out")
    }

    fn put(&self, rel: &str, v: &Value) {
        let p = self.cache().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, v.to_string()).unwrap();
    }

    fn opts(&self, h: &Hosts) -> catalog::Options {
        catalog::Options {
            cache: self.cache(),
            ours: self.ours(),
            out: self.out(),
            through: super::latest(),
            rights: None,
            hosts: h.clone(),
            fetch: None,
        }
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.out().join(name)).unwrap()
    }

    fn decisions(&self) -> Vec<(String, String)> {
        self.read("titles.tsv")
            .lines()
            .skip(1)
            .map(|l| {
                let f: Vec<_> = l.split('\t').collect();
                (f[0].to_owned(), f[2].to_owned())
            })
            .collect()
    }
}

async fn catalogue(o: catalog::Options) -> anyhow::Result<catalog::Summary> {
    let (_tx, rx) = no_stop();
    catalog::run(o, rx).await
}

#[tokio::test]
async fn the_catalogue_selects_titles_we_lack_up_to_1930_and_enqueues_nothing() {
    let h = Hosts::ghn();
    let c = Cat::new(&h);
    let s = catalogue(c.opts(&h)).await.unwrap();
    let want = [
        (OURS, "ours"),
        (LACK, "selected"),
        (LATE, "none-in-range"),
        (BARE, "no-metadata"),
        (WIREGRASS, "elsewhere"),
    ];
    let mut want: Vec<_> = want
        .iter()
        .map(|(l, d)| (l.to_string(), d.to_string()))
        .collect();
    want.sort();
    assert_eq!(c.decisions(), want);
    let issues = c.read("issues.tsv");
    let rows: Vec<_> = issues.lines().collect();
    assert_eq!(rows[0], catalog::ISSUES_HEADER);
    assert_eq!(
        rows[1..],
        [
            format!(
                "{LACK}\t1929-05-01\t1\t{}\tunchecked",
                issue(LACK, "1929-05-01", 1).url(&h)
            ),
            format!(
                "{LACK}\t1930-12-31\t2\t{}\tunchecked",
                issue(LACK, "1930-12-31", 2).url(&h)
            ),
        ]
    );
    assert!(s.dry_run);
    assert_eq!(s.enqueued, 0);
    assert!(!s.rights_checked);
    assert_eq!(s.issues_selected, 2);
    assert_eq!(s.titles[&Decision::Selected], 1);
    assert_eq!(s.titles[&Decision::Elsewhere], 1);
    let saved: Value = serde_json::from_str(&c.read("summary.json")).unwrap();
    assert_eq!(saved["dry_run"], true);
    assert_eq!(saved["titles"]["none-in-range"], 1);

    // An earlier cut-off takes less; a later one isn't ours to make.
    let mut o = c.opts(&h);
    o.through = day("1929-12-31");
    assert_eq!(catalogue(o).await.unwrap().issues_selected, 1);
    let mut o = c.opts(&h);
    o.through = day("1931-01-01");
    let e = catalogue(o).await.unwrap_err().to_string();
    assert!(e.contains("after 1930-12-31"), "{e}");
}

#[tokio::test]
async fn with_rights_only_noc_us_issues_are_selected() {
    let h = Hosts::ghn();
    let c = Cat::new(&h);
    let rights = c.dir.path().join("rights.tsv");
    let noc = "http://rightsstatements.org/vocab/NoC-US/1.0/";
    let inc = "http://rightsstatements.org/vocab/InC/1.0/";
    std::fs::write(
        &rights,
        format!("lccn\tdate\trights\n{LACK}\t1929-05-01\t{noc}\n{LACK}\t1930-12-31\t{inc}\n"),
    )
    .unwrap();
    let mut o = c.opts(&h);
    o.rights = Some(rights);
    let s = catalogue(o).await.unwrap();
    assert!(s.rights_checked);
    assert_eq!((s.issues_selected, s.issues_without_noc_us), (1, 1));
    assert!(c.read("issues.tsv").lines().nth(1).unwrap().ends_with(noc));
}

#[tokio::test]
async fn without_a_cache_or_leave_to_ask_the_catalogue_asks_nothing() {
    let h = Hosts::ghn();
    let c = Cat::new(&h);
    std::fs::remove_file(c.cache().join("newspapers.json")).unwrap();
    let e = catalogue(c.opts(&h)).await.unwrap_err().to_string();
    assert!(e.contains("--fetch-metadata"), "{e}");
}

#[tokio::test]
async fn the_catalogue_asks_for_missing_metadata_within_ghns_policy() {
    let s = StandIn::start().await;
    let h = hosts(&s);
    let c = Cat {
        dir: tempfile::tempdir().unwrap(),
    };
    std::fs::write(
        c.ours(),
        json!([{"curated": {"lccns": [OURS]}}]).to_string(),
    )
    .unwrap();
    let list = newspapers_doc(&h, &[OURS, LACK, BARE, WIREGRASS]);
    s.put("/newspapers.json", list.to_string());
    s.put(
        &format!("/lccn/{LACK}.json"),
        title_doc(&h, LACK, &[("1929-05-01", 1)]).to_string(),
    );
    let mut o = c.opts(&h);
    o.fetch = Some(catalog::Fetch {
        pacer: c.dir.path().join("ghn-pacer.json"),
        policies: policies(),
        max_titles: None,
    });
    let sum = catalogue(o).await.unwrap();
    assert_eq!(sum.collections_fetched, 1);
    assert_eq!(sum.issues_selected, 1);
    assert_eq!(s.hits("/newspapers.json"), 1);
    assert_eq!(
        s.hits(&format!("/lccn/{BARE}.json")),
        1,
        "a 404 is asked once"
    );
    assert_eq!(
        s.hits(&format!("/lccn/{OURS}.json")),
        0,
        "ours isn't asked for"
    );
    assert_eq!(s.hits(&format!("/lccn/{WIREGRASS}.json")), 0);
    assert!(c.cache().join(format!("lccn/{LACK}.json")).exists());
    assert!(!c.cache().join(format!("lccn/{BARE}.json")).exists());
    assert_eq!(
        c.decisions()
            .iter()
            .find(|(l, _)| l == BARE)
            .map(|(_, d)| d.as_str()),
        Some("no-metadata")
    );
}

/// GHN's Turnstile challenge comes as a redirect off the allowed paths: it's push-back, the
/// challenge is never asked for, and GHN's budget is slowed for whoever asks next.
#[tokio::test]
async fn a_challenge_redirect_is_push_back_and_never_followed() {
    let s = StandIn::start().await;
    let h = hosts(&s);
    let c = Cat {
        dir: tempfile::tempdir().unwrap(),
    };
    std::fs::write(
        c.ours(),
        json!([{"curated": {"lccns": [OURS]}}]).to_string(),
    )
    .unwrap();
    s.put(
        "/newspapers.json",
        newspapers_doc(&h, &[LACK, BARE]).to_string(),
    );
    // Titles are asked for in LCCN order: LACK, then BARE.
    s.redirect(
        &format!("/lccn/{LACK}.json"),
        "/cdn-cgi/challenge-platform/h/b/turnstile",
    );
    s.put(
        &format!("/lccn/{BARE}.json"),
        title_doc(&h, BARE, &[("1929-05-01", 1)]).to_string(),
    );
    let mut o = c.opts(&h);
    o.fetch = Some(catalog::Fetch {
        pacer: c.dir.path().join("ghn-pacer.json"),
        policies: policies(),
        max_titles: None,
    });
    let e = catalogue(o.clone()).await.unwrap_err().to_string();
    assert!(e.contains("pushed back"), "{e}");
    assert_eq!(s.hits_under("/cdn-cgi/"), 0);
    assert_eq!(s.hits(&format!("/lccn/{BARE}.json")), 0, "nothing after it");
    let e = catalogue(o).await.unwrap_err().to_string();
    assert!(e.contains("slow"), "{e}");
    assert_eq!(s.hits("/newspapers.json"), 1);
}

/// The catalogue's documents come through the engine as files, so GHN's byte cap paces
/// them as it does images, and one that won't parse is never kept.
#[tokio::test]
async fn the_catalogues_documents_keep_to_ghns_byte_cap() {
    let s = StandIn::start().await;
    let h = hosts(&s);
    let c = Cat {
        dir: tempfile::tempdir().unwrap(),
    };
    std::fs::write(
        c.ours(),
        json!([{"curated": {"lccns": [OURS]}}]).to_string(),
    )
    .unwrap();
    let mut list = newspapers_doc(&h, &[LACK, BARE]);
    list["description"] = json!("x".repeat(60_000));
    s.put("/newspapers.json", list.to_string());
    s.put(
        &format!("/lccn/{LACK}.json"),
        title_doc(&h, LACK, &[("1929-05-01", 1)]).to_string(),
    );
    s.put(&format!("/lccn/{BARE}.json"), "{\"not\": \"a collection\"}");
    let mut p = ghn_policy();
    p.max_bytes_per_sec = Some(30_000);
    let mut o = c.opts(&h);
    o.fetch = Some(catalog::Fetch {
        pacer: c.dir.path().join("ghn-pacer.json"),
        policies: Policies::new(vec![p]).unwrap(),
        max_titles: None,
    });
    let t = std::time::Instant::now();
    let sum = catalogue(o).await.unwrap();
    assert!(
        t.elapsed() >= Duration::from_millis(1900),
        "{:?}",
        t.elapsed()
    );
    assert_eq!((sum.collections_fetched, sum.issues_selected), (1, 1));
    let mut kept: Vec<String> = std::fs::read_dir(c.cache().join("lccn"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    kept.sort();
    assert_eq!(kept, [format!("{LACK}.json")], "nothing partial or refused");
    assert!(c.cache().join("newspapers.json").exists());
}

// ---- the fetch ----

struct Rig {
    s: StandIn,
    h: Hosts,
    dir: tempfile::TempDir,
    rows: Vec<String>,
}

impl Rig {
    async fn new() -> Self {
        let s = StandIn::start().await;
        let h = hosts(&s);
        Rig {
            s,
            h,
            dir: tempfile::tempdir().unwrap(),
            rows: vec![catalog::ISSUES_HEADER.to_owned()],
        }
    }

    /// An issue whose pages are `(seq, body)`, listed with `rights`.
    fn issue(&mut self, i: &IssueRef, pages: &[(u16, &[u8])], rights: &str) {
        let idents: Vec<String> = pages
            .iter()
            .map(|(seq, _)| format!("batch_x%2Fdata%2F{}%2F{:04}.jp2", i.stem(), seq))
            .collect();
        let shape: Vec<(u16, &str)> = pages
            .iter()
            .zip(&idents)
            .map(|((seq, _), id)| (*seq, id.as_str()))
            .collect();
        let doc = issue_doc(&self.h, i, &shape);
        self.s.put(&path_of(&i.url(&self.h)), doc.to_string());
        for ((_, body), id) in pages.iter().zip(&idents) {
            let url = format!("{}{id}/full/full/0/default.jpg", self.h.images);
            self.s.put(&path_of(&url), body.to_vec());
        }
        self.rows.push(format!(
            "{}\t{}\t{}\t{}\t{rights}",
            i.lccn,
            i.date,
            i.edition,
            i.url(&self.h)
        ));
    }

    fn out(&self) -> PathBuf {
        self.dir.path().join("out")
    }

    fn opts(&self) -> fetch::Options {
        let issues = self.dir.path().join("issues.tsv");
        std::fs::write(&issues, self.rows.join("\n") + "\n").unwrap();
        fetch::Options {
            issues,
            out: self.out(),
            pacer: self.dir.path().join("ghn-pacer.json"),
            policies: policies(),
            hosts: self.h.clone(),
            max_hours: 1.0,
            min_free_bytes: 0,
            limit: None,
            unchecked_rights: true,
        }
    }

    fn page(&self, i: &IssueRef, seq: u16) -> (PathBuf, PathBuf) {
        let dir = self
            .out()
            .join("pages")
            .join(&i.lccn)
            .join(i.date.format("%Y").to_string());
        let doc = format!("{}_seq-{seq}", i.stem());
        (
            dir.join(format!("{doc}.jpg")),
            dir.join(format!("{doc}.json")),
        )
    }

    fn sidecar(&self, i: &IssueRef, seq: u16) -> Sidecar {
        serde_json::from_slice(&std::fs::read(self.page(i, seq).1).unwrap()).unwrap()
    }
}

async fn fetch_all(o: fetch::Options) -> anyhow::Result<crate::fetcher::Status> {
    let (_tx, rx) = no_stop();
    tokio::time::timeout(Duration::from_secs(30), fetch::run(o, rx))
        .await
        .expect("the fetch finishes")
}

fn images_asked(r: &Rig) -> usize {
    r.s.hits("/default.jpg")
}

#[tokio::test]
async fn pages_come_after_their_manifest_with_ids_provenance_and_checksums() {
    let mut r = Rig::new().await;
    let i = issue(LACK, "1929-05-01", 1);
    let p2: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE1, 9, 9];
    r.issue(&i, &[(1, JPEG), (2, p2)], "unchecked");
    let s = fetch_all(r.opts()).await.unwrap();
    assert_eq!(s.state, "done");
    assert_eq!(s.fetched_this_run, 3, "a manifest and two pages");
    assert_eq!(std::fs::read(r.page(&i, 1).0).unwrap(), JPEG);
    let manifest = r.s.file(&path_of(&i.url(&r.h))).unwrap();
    let side = r.sidecar(&i, 2);
    assert_eq!(side.page_id, format!("ghn:{LACK}_1929-05-01_ed-1_seq-2"));
    assert_eq!(side.source, "ghn");
    assert_eq!((side.seq, side.edition), (2, 1));
    assert_eq!(side.sha256, sha(p2));
    assert_eq!(side.bytes, p2.len() as u64);
    assert_eq!(side.manifest_url, i.url(&r.h));
    assert_eq!(side.manifest_sha256, sha(&manifest));
    assert!(side
        .image_url
        .ends_with("%2F0002.jp2/full/full/0/default.jpg"));
    assert_eq!(side.rights, "unchecked");
    assert_eq!(side.published_through, "1930-12-31");
    assert_eq!((side.width, side.height), (Some(50), Some(70)));
    let status: Value =
        serde_json::from_slice(&std::fs::read(r.out().join("ghn-status.json")).unwrap()).unwrap();
    assert_eq!(status["state"], "done");

    // A rerun asks nothing; a page without its sidecar is asked for again.
    let s = fetch_all(r.opts()).await.unwrap();
    assert_eq!((s.state.as_str(), s.fetched_this_run), ("done", 0));
    assert_eq!(images_asked(&r), 2);
    std::fs::remove_file(r.page(&i, 1).1).unwrap();
    let s = fetch_all(r.opts()).await.unwrap();
    assert_eq!((s.state.as_str(), s.fetched_this_run), ("done", 1));
    assert_eq!(images_asked(&r), 3);
    assert_eq!(r.s.hits(".json"), 1, "the manifest kept on disk is used");
}

#[tokio::test]
async fn something_other_than_a_jpeg_is_removed_and_left_to_do() {
    let mut r = Rig::new().await;
    let i = issue(LACK, "1929-05-01", 1);
    r.issue(&i, &[(1, b"<html>not a page</html>")], "unchecked");
    let s = fetch_all(r.opts()).await.unwrap();
    assert_eq!(s.state, "incomplete");
    let (jpg, json) = r.page(&i, 1);
    assert!(!jpg.exists() && !json.exists());
    let problems = std::fs::read_to_string(r.out().join("ghn-problems.tsv")).unwrap();
    assert!(problems.contains("not a JPEG"), "{problems}");
}

#[tokio::test]
async fn a_push_back_on_a_page_ends_the_run_and_slows_ghn_for_the_next() {
    let mut r = Rig::new().await;
    let i = issue(LACK, "1929-05-01", 1);
    r.issue(&i, &[(1, JPEG), (2, JPEG)], "unchecked");
    let first = format!(
        "{}batch_x%2Fdata%2F{}%2F0001.jp2/full/full/0/default.jpg",
        r.h.images,
        i.stem()
    );
    r.s.script(&path_of(&first), Scripted::Challenge(403));
    let e = fetch_all(r.opts()).await.unwrap_err().to_string();
    assert!(e.contains("pushed back"), "{e}");
    assert_eq!(images_asked(&r), 1, "nothing after the push-back");
    let e = fetch_all(r.opts()).await.unwrap_err().to_string();
    assert!(e.contains("slow"), "{e}");
    assert_eq!(images_asked(&r), 1);
}

/// A sidecar describes the bytes it was written for: with its image gone it's removed before
/// the page is asked for again, so a replacement that fails can't look done.
#[tokio::test]
async fn a_sidecar_whose_image_is_gone_is_removed_before_the_page_is_asked_again() {
    let mut r = Rig::new().await;
    let i = issue(LACK, "1929-05-01", 1);
    r.issue(&i, &[(1, JPEG)], "unchecked");
    assert_eq!(fetch_all(r.opts()).await.unwrap().state, "done");
    let (jpg, json) = r.page(&i, 1);
    std::fs::remove_file(&jpg).unwrap();
    let image = format!(
        "{}batch_x%2Fdata%2F{}%2F0001.jp2/full/full/0/default.jpg",
        r.h.images,
        i.stem()
    );
    r.s.script(&path_of(&image), Scripted::Status(404));
    let s = fetch_all(r.opts()).await.unwrap();
    assert_eq!(s.state, "incomplete");
    assert_eq!(images_asked(&r), 2);
    assert!(!json.exists() && !jpg.exists());
}

#[tokio::test]
async fn the_limit_counts_issues_started() {
    let mut r = Rig::new().await;
    let a = issue(LACK, "1929-05-01", 1);
    let b = issue(LACK, "1929-05-02", 1);
    r.issue(&a, &[(1, JPEG)], "unchecked");
    r.issue(&b, &[(1, JPEG)], "unchecked");
    let mut o = r.opts();
    o.limit = Some(1);
    let s = fetch_all(o).await.unwrap();
    assert_eq!(s.state, "limit");
    assert!(r.page(&a, 1).1.exists());
    assert_eq!(r.s.hits(&path_of(&b.url(&r.h))), 0);
}

#[test]
fn the_issue_list_is_checked_before_anything_is_asked() {
    let h = Hosts::ghn();
    let noc = "http://rightsstatements.org/vocab/NoC-US/1.0/";
    let row = |i: &IssueRef, rights: &str| {
        format!(
            "{}\n{}\t{}\t{}\t{}\t{rights}\n",
            catalog::ISSUES_HEADER,
            i.lccn,
            i.date,
            i.edition,
            i.url(&h)
        )
    };
    let read = |t: &str, unchecked: bool| fetch::read_issues(t, &h, unchecked);
    let ok = issue(LACK, "1930-12-31", 1);
    assert_eq!(read(&row(&ok, noc), false).unwrap().len(), 1);
    let e = read(&row(&ok, "unchecked"), false).unwrap_err().to_string();
    assert!(e.contains("--unchecked-rights"), "{e}");
    assert!(read(&row(&ok, "unchecked"), true).is_ok());
    assert!(read(
        &row(&ok, "http://rightsstatements.org/vocab/InC/1.0/"),
        true
    )
    .is_err());
    assert!(read(&row(&issue(LACK, "1931-01-01", 1), noc), false).is_err());
    let e = read(&row(&issue(WIREGRASS, "1920-01-01", 1), noc), false)
        .unwrap_err()
        .to_string();
    assert!(e.contains("#1419"), "{e}");
    // The URL has to be the row's issue, on GHN.
    let other = row(&ok, noc).replace("1930-12-31\t1\t", "1930-12-30\t1\t");
    assert!(read(&other, false).is_err());
    let elsewhere = row(&ok, noc).replace(&h.site, "https://elsewhere.example");
    assert!(read(&elsewhere, false).is_err());
}
