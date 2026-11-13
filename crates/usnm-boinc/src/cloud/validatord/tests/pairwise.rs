//! Differential tests of the pairwise apps: the service against the stock library init and compare
//! functions, driven the way `check_set` and `check_pair` drive them (candidate first, stop at the
//! first candidate with `min_quorum / 2` matches; a late result against the canonical one). The
//! server's own decision from a `matches` reply is modelled by `decide`, from D39 in cloud-boinc
//! `cloud/docs/compatibility.md`. Mock HTTP only.
use super::*;
use std::collections::BTreeSet;
use usnm_ocr_contract::quality::ResultManifest;

type Files = Vec<(String, Vec<u8>)>;

#[derive(Clone, Copy)]
enum App {
    Ndlocr,
    Quality,
}

impl App {
    fn name(self) -> &'static str {
        match self {
            App::Ndlocr => "usnm_ndlocr",
            App::Quality => "usnm_quality",
        }
    }
    /// What the stock init script runs (with `--keep` for quality).
    fn init(self, f: &[PathBuf], keep: &Path) -> bool {
        match self {
            App::Ndlocr => crate::ndlocr::load(f).is_ok(),
            App::Quality => crate::quality::validate::check(f, Some(keep)).is_ok(),
        }
    }
    /// What the stock compare script runs (with `--log` for quality).
    fn compare(self, a: &[PathBuf], b: &[PathBuf], log: &Path) -> bool {
        match self {
            App::Ndlocr => crate::ndlocr::compare(a, b).is_ok(),
            App::Quality => crate::quality::validate::compare(a, b, Some(log)).is_ok(),
        }
    }
}

// ---- fixtures -------------------------------------------------------------------------------

fn named(app: App, tag: &str, bytes: [Vec<u8>; 3]) -> Files {
    bytes
        .into_iter()
        .enumerate()
        .map(|(i, b)| (format!("{}_x_{tag}_{i}", app.name()), b))
        .collect()
}

/// The three outputs of a synthetic quality unit, as the stock validator's own tests make them.
async fn quality_base() -> [Vec<u8>; 3] {
    let dir = tempfile::tempdir().unwrap();
    crate::quality::work::tests::unit(dir.path());
    crate::quality::work::run(dir.path()).await.unwrap();
    let mut it = crate::quality::outputs()
        .into_iter()
        .map(|n| std::fs::read(dir.path().join(n)).unwrap());
    [it.next().unwrap(), it.next().unwrap(), it.next().unwrap()]
}

fn edited(base: &[Vec<u8>; 3], f: impl FnOnce(&mut ResultManifest)) -> [Vec<u8>; 3] {
    let mut r: ResultManifest = serde_json::from_slice(&base[0]).unwrap();
    f(&mut r);
    [
        serde_json::to_vec(&r).unwrap(),
        base[1].clone(),
        base[2].clone(),
    ]
}

/// Agrees with the base: another host and another time.
fn other_host(base: &[Vec<u8>; 3]) -> [Vec<u8>; 3] {
    edited(base, |r| {
        r.host.ncpus = Some(4.0);
        r.timings.total_ms += 1000;
    })
}

/// A different record: does not agree.
fn other_record(base: &[Vec<u8>; 3]) -> [Vec<u8>; 3] {
    edited(base, |r| r.readers[0].words += 1)
}

fn tampered(base: &[Vec<u8>; 3]) -> [Vec<u8>; 3] {
    [base[0].clone(), b"tampered".to_vec(), base[2].clone()]
}

fn nd_job() -> crate::ndlocr::Job {
    use usnm_ocr_contract::boinc::{sha256, Scan};
    let mut job = crate::ndlocr::Job {
        v: 1,
        id: String::new(),
        page: "sn83025517/1945-01-01/ed-1/seq-4".into(),
        batch: "dlc_ballston".into(),
        batch_version: 1,
        loc_text: "mixed".into(),
        route: usnm_ocr_worker::engine::candidates("jpn").remove(0),
        engine: crate::ndlocr::engine(format!("sha256:{}", "a".repeat(64)), 600),
        scan: Scan {
            url: "https://tile.loc.gov/image/full/full/0/default.jpg".into(),
            iiif_base: "https://tile.loc.gov/image".into(),
            sha256: sha256(b"scan"),
            bytes: 4,
            md5: None,
        },
    };
    job.id = job.identity().unwrap();
    job
}

/// text, ALTO and result.json of one Japanese result, in template order.
fn nd_outputs(text: &str) -> [Vec<u8>; 3] {
    use usnm_ocr_adapter::ndlocr::{ndlocr_to_alto, normalize, Geometry, COMMIT};
    use usnm_ocr_contract::boinc::sha256;
    let job = nd_job();
    let lines: String = text
        .lines()
        .map(|l| format!("<LINE X=\"10\" Y=\"10\" WIDTH=\"900\" HEIGHT=\"100\" STRING=\"{l}\" CONF=\"0.95\"/>"))
        .collect();
    let xml = format!(
        "<OCRDATASET><PAGE WIDTH=\"1000\" HEIGHT=\"2000\"><TEXTBLOCK>{lines}</TEXTBLOCK></PAGE></OCRDATASET>"
    );
    let alto = ndlocr_to_alto(
        xml.as_bytes(),
        Geometry::default(),
        &job.scan.url,
        COMMIT,
        1,
        "2026-10-16T00:00:00+00:00",
    )
    .unwrap();
    let text = normalize(text);
    let r = crate::ndlocr::ResultManifest {
        job,
        text_sha256: sha256(text.as_bytes()),
        alto_sha256: sha256(&alto),
        elapsed_ms: 500,
        completed_at: "2026-10-16T00:00:00+00:00".into(),
    };
    [text.into_bytes(), alto, serde_json::to_vec(&r).unwrap()]
}

fn nd_text() -> String {
    vec!["日本語新聞記事を読みます。"; 80].join("\n")
}

/// Registers a result's files on the mock (unless `register` is false: the output is then not
/// there and its fetch fails) and returns the request's file entries.
fn entries(fx: &Fx, files: &Files, register: bool) -> Vec<Value> {
    files
        .iter()
        .enumerate()
        .map(|(i, (name, bytes))| {
            if register {
                fx.mock
                    .outputs
                    .lock()
                    .unwrap()
                    .insert(name.clone(), bytes.clone());
            }
            // The same entry either way: a call id belongs to its first request.
            json!({"name": name, "open_name": null, "index": i,
                   "md5": md5_hex(bytes), "nbytes": bytes.len()})
        })
        .collect()
}

// ---- the stock side -------------------------------------------------------------------------

struct Stock {
    /// valid / invalid / undecided per result (the canonical result is left out of `check_pair`).
    states: BTreeMap<i64, &'static str>,
    pairs: BTreeSet<(i64, i64)>,
    /// Stock's own files: `seen/` and `pairs.jsonl`.
    dir: tempfile::TempDir,
}

fn pair(a: i64, b: i64) -> (i64, i64) {
    (a.min(b), a.max(b))
}

/// `check_set` or `check_pair` as `sched/validate_util2.cpp` runs them over the library's init
/// and compare. `cases` are in the workunit's result order.
fn stock(
    app: App,
    cases: &[(i64, &Files)],
    mode: &str,
    quorum: i64,
    canonical: Option<i64>,
) -> Stock {
    let dir = tempfile::tempdir().unwrap();
    let (keep, log) = (dir.path().join("seen"), dir.path().join("pairs.jsonl"));
    let mut paths = Vec::new();
    for (id, files) in cases {
        let d = dir.path().join("in").join(id.to_string());
        std::fs::create_dir_all(&d).unwrap();
        let ps: Vec<PathBuf> = files
            .iter()
            .map(|(n, b)| {
                std::fs::write(d.join(n), b).unwrap();
                d.join(n)
            })
            .collect();
        paths.push(ps);
    }
    let ok: Vec<bool> = paths.iter().map(|p| app.init(p, &keep)).collect();
    let mut states: BTreeMap<i64, &'static str> = BTreeMap::new();
    let mut pairs = BTreeSet::new();
    for (i, (id, _)) in cases.iter().enumerate() {
        states.insert(*id, if ok[i] { "undecided" } else { "invalid" });
    }
    let usable: Vec<usize> = (0..cases.len()).filter(|i| ok[*i]).collect();
    if mode == "check_pair" {
        let c = cases
            .iter()
            .position(|(id, _)| Some(*id) == canonical)
            .unwrap();
        states.remove(&cases[c].0);
        if ok[c] {
            for &l in usable.iter().filter(|l| **l != c) {
                let m = app.compare(&paths[l], &paths[c], &log);
                if m {
                    pairs.insert(pair(cases[l].0, cases[c].0));
                }
                states.insert(cases[l].0, if m { "valid" } else { "invalid" });
            }
        }
    } else if usable.len() as i64 >= quorum {
        for &i in &usable {
            let mut matched = Vec::new();
            for &j in usable.iter().filter(|j| **j != i) {
                if app.compare(&paths[i], &paths[j], &log) {
                    matched.push(j);
                    pairs.insert(pair(cases[i].0, cases[j].0));
                }
            }
            if matched.len() as i64 >= quorum / 2 {
                for &j in &usable {
                    states.insert(
                        cases[j].0,
                        if j == i || matched.contains(&j) {
                            "valid"
                        } else {
                            "invalid"
                        },
                    );
                }
                break;
            }
        }
    }
    Stock { states, pairs, dir }
}

// ---- the server side ------------------------------------------------------------------------

/// What the cloud server does with a reply (D39, D41): returns the same kind of map as `stock`.
fn decide(
    reply: &Value,
    mode: &str,
    quorum: i64,
    canonical: Option<i64>,
) -> BTreeMap<i64, &'static str> {
    let listed: BTreeSet<(i64, i64)> = reply["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| pair(p[0].as_i64().unwrap(), p[1].as_i64().unwrap()))
        .collect();
    let mut states = BTreeMap::new();
    let mut usable = Vec::new();
    let mut retry = false;
    for r in reply["results"].as_array().unwrap() {
        let id = r["id"].as_i64().unwrap();
        assert!(
            r.get("fingerprint").is_none(),
            "pairwise form has no fingerprints"
        );
        if r["invalid"] == true {
            states.insert(id, "invalid");
        } else if r["retry"] == true {
            retry = true;
            states.insert(id, "undecided");
        } else {
            states.insert(id, "undecided");
            usable.push(id);
        }
    }
    if mode == "check_pair" {
        let c = canonical.unwrap();
        states.remove(&c);
        if !retry || usable.contains(&c) {
            for l in usable.iter().filter(|l| **l != c) {
                states.insert(
                    *l,
                    if listed.contains(&pair(*l, c)) {
                        "valid"
                    } else {
                        "invalid"
                    },
                );
            }
        }
        return states;
    }
    if retry || (usable.len() as i64) < quorum {
        return states;
    }
    for &i in &usable {
        let n = usable
            .iter()
            .filter(|j| **j != i && listed.contains(&pair(i, **j)))
            .count() as i64;
        if n + 1 > quorum / 2 {
            for &j in &usable {
                states.insert(
                    j,
                    if j == i || listed.contains(&pair(i, j)) {
                        "valid"
                    } else {
                        "invalid"
                    },
                );
            }
            break;
        }
    }
    states
}

fn matches_of(reply: &Value) -> BTreeSet<(i64, i64)> {
    reply["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| pair(p[0].as_i64().unwrap(), p[1].as_i64().unwrap()))
        .collect()
}

fn pbody(
    call_id: &str,
    app: App,
    mode: &str,
    quorum: i64,
    canonical: Option<i64>,
    results: Vec<Value>,
) -> Value {
    json!({"call_id": call_id, "mode": mode,
           "workunit": {"id": 1, "name": "wu", "app": app.name(), "batch": 0, "min_quorum": quorum},
           "canonical_result_id": canonical, "results": results})
}

/// A pair log with the times taken out, and the files of a directory tree, to compare.
fn log_lines(path: &Path) -> Vec<Value> {
    match std::fs::read_to_string(path) {
        Ok(s) => s
            .lines()
            .map(|l| {
                let mut v: Value = serde_json::from_str(l).unwrap();
                v.as_object_mut().unwrap().remove("at");
                v
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                out.insert(
                    p.strip_prefix(root).unwrap().to_string_lossy().into_owned(),
                    std::fs::read(&p).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

struct Run<'a> {
    fx: &'a Fx,
    base: String,
    app: App,
}

impl Run<'_> {
    async fn new(fx: &Fx, app: App) -> Run<'_> {
        Run {
            fx,
            base: fx.serve().await,
            app,
        }
    }

    async fn post(
        &self,
        call_id: &str,
        mode: &str,
        quorum: i64,
        canonical: Option<i64>,
        cases: &[(i64, &Files)],
        register: bool,
    ) -> (u16, Value) {
        let results = cases
            .iter()
            .map(|(id, f)| res(*id, entries(self.fx, f, register)))
            .collect();
        post(
            &self.base,
            self.app.name(),
            Some("hook"),
            &pbody(call_id, self.app, mode, quorum, canonical, results),
        )
        .await
    }

    /// Posts, then checks the verdicts, the pairs and the side effects against the stock run.
    async fn agree(
        &self,
        call_id: &str,
        mode: &str,
        quorum: i64,
        canonical: Option<i64>,
        cases: &[(i64, &Files)],
    ) -> Value {
        let (status, reply) = self
            .post(call_id, mode, quorum, canonical, cases, true)
            .await;
        assert_eq!(status, 200, "{reply}");
        let want = stock(self.app, cases, mode, quorum, canonical);
        assert_eq!(
            decide(&reply, mode, quorum, canonical),
            want.states,
            "{reply}"
        );
        assert_eq!(matches_of(&reply), want.pairs, "{reply}");
        reply
    }

    fn quality_dir(&self) -> PathBuf {
        self.fx.state.path().join("quality")
    }

    /// The service's side-effect files equal stock's, in the same order.
    fn same_files_as(&self, stock_dir: &Path) {
        let q = self.quality_dir();
        assert_eq!(
            log_lines(&q.join("pairs.jsonl")),
            log_lines(&stock_dir.join("pairs.jsonl"))
        );
        assert_eq!(tree(&q.join("seen")), tree(&stock_dir.join("seen")));
    }
}

// ---- tests ----------------------------------------------------------------------------------

#[tokio::test]
async fn quality_matches_the_stock_check_set_on_agreeing_and_differing_results() {
    let fx = Fx::new().await;
    let base = quality_base().await;
    let a = named(App::Quality, "a", base.clone());
    let b = named(App::Quality, "b", other_host(&base));
    let c = named(App::Quality, "c", other_record(&base));
    let bad = named(App::Quality, "bad", tampered(&base));
    let run = Run::new(&fx, App::Quality).await;
    // Agree: a is canonical at once, with b.
    let r = run
        .agree(
            "wu1-rev1-check_set",
            "check_set",
            2,
            None,
            &[(1, &a), (2, &b)],
        )
        .await;
    assert_eq!(matches_of(&r), BTreeSet::from([(1, 2)]));
    // Differ: no candidate has a match, so both are compared (a first, then b) and nothing is
    // decided. Stock's log holds both orders.
    let r = run
        .agree(
            "wu2-rev1-check_set",
            "check_set",
            2,
            None,
            &[(3, &a), (4, &c)],
        )
        .await;
    assert!(matches_of(&r).is_empty());
    // An unreadable output is invalid and out of the comparison; the others still agree.
    let r = run
        .agree(
            "wu3-rev1-check_set",
            "check_set",
            2,
            None,
            &[(5, &bad), (6, &a), (7, &b)],
        )
        .await;
    assert_eq!(r["results"][0]["invalid"], true);
    // Three results, the first one odd: the first candidate has no match, the second has two.
    run.agree(
        "wu4-rev1-check_set",
        "check_set",
        2,
        None,
        &[(8, &c), (9, &a), (10, &b)],
    )
    .await;
    // Quorum 1: the first result is canonical with nothing to compare it with.
    run.agree("wu5-rev1-check_set", "check_set", 1, None, &[(11, &a)])
        .await;
    // The side effects over all of the above, in order.
    let cases: Vec<(i64, Vec<(i64, &Files)>)> = vec![
        (2, vec![(1, &a), (2, &b)]),
        (2, vec![(3, &a), (4, &c)]),
        (2, vec![(5, &bad), (6, &a), (7, &b)]),
        (2, vec![(8, &c), (9, &a), (10, &b)]),
        (1, vec![(11, &a)]),
    ];
    let dir = tempfile::tempdir().unwrap();
    // One stock run over all five, appending to one log, as the daemon would.
    let (keep, log) = (dir.path().join("seen"), dir.path().join("pairs.jsonl"));
    for (q, cs) in &cases {
        let one = stock(App::Quality, cs, "check_set", *q, None);
        for l in log_lines(&one.dir.path().join("pairs.jsonl")) {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
                .unwrap();
            use std::io::Write as _;
            writeln!(f, "{l}").unwrap();
        }
        for (k, v) in tree(&one.dir.path().join("seen")) {
            let p = keep.join(k);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, v).unwrap();
        }
    }
    run.same_files_as(dir.path());
    assert!(log_lines(&log).iter().any(|l| l["agree"] == false));
}

#[tokio::test]
async fn quality_check_pair_compares_only_late_against_canonical_and_a_third_result_arrives_late() {
    let fx = Fx::new().await;
    let base = quality_base().await;
    let a = named(App::Quality, "a", base.clone());
    let b = named(App::Quality, "b", other_host(&base));
    let late_ok = named(
        App::Quality,
        "d",
        edited(&base, |r| r.timings.total_ms += 7),
    );
    let late_bad = named(App::Quality, "c", other_record(&base));
    let run = Run::new(&fx, App::Quality).await;
    run.agree(
        "wu1-rev1-check_set",
        "check_set",
        2,
        None,
        &[(1, &a), (2, &b)],
    )
    .await;
    // Result 1 is canonical. Two late results: one agrees, one does not. The two late results
    // are never compared with each other.
    let r = run
        .agree(
            "wu1-rev2-check_pair",
            "check_pair",
            2,
            Some(1),
            &[(3, &late_bad), (4, &late_ok), (1, &a)],
        )
        .await;
    assert_eq!(matches_of(&r), BTreeSet::from([(1, 4)]));
    // Stock's log, in order: set (a,b) then pair (c,a) (d,a).
    let log = log_lines(&run.quality_dir().join("pairs.jsonl"));
    assert_eq!(log.len(), 3);
    assert_eq!(log[0]["a"]["result"], "usnm_quality_x_a_0");
    assert_eq!(log[1]["a"]["result"], "usnm_quality_x_c_0");
    assert_eq!(log[1]["b"]["result"], "usnm_quality_x_a_0");
    assert_eq!(log[1]["agree"], false);
    assert_eq!(log[2]["a"]["result"], "usnm_quality_x_d_0");
    assert_eq!(log[2]["agree"], true);
}

#[tokio::test]
async fn a_replayed_call_id_adds_nothing_and_a_new_call_appends_like_stock_does() {
    let fx = Fx::new().await;
    let base = quality_base().await;
    let a = named(App::Quality, "a", base.clone());
    let b = named(App::Quality, "b", other_host(&base));
    let run = Run::new(&fx, App::Quality).await;
    let cases: &[(i64, &Files)] = &[(1, &a), (2, &b)];
    let first = run
        .agree("wu1-rev1-check_set", "check_set", 2, None, cases)
        .await;
    let (log, seen) = (
        std::fs::read(run.quality_dir().join("pairs.jsonl")).unwrap(),
        tree(&run.quality_dir().join("seen")),
    );
    let fetched = fx.mock.fetches.load(Ordering::SeqCst);
    let again = run
        .post("wu1-rev1-check_set", "check_set", 2, None, cases, true)
        .await;
    assert_eq!(again, (200, first.clone()));
    // After a restart too.
    let run2 = Run::new(&fx, App::Quality).await;
    let once_more = run2
        .post("wu1-rev1-check_set", "check_set", 2, None, cases, true)
        .await;
    assert_eq!(once_more, (200, first));
    assert_eq!(fx.mock.fetches.load(Ordering::SeqCst), fetched);
    assert_eq!(
        std::fs::read(run.quality_dir().join("pairs.jsonl")).unwrap(),
        log
    );
    assert_eq!(tree(&run.quality_dir().join("seen")), seen);
    // The same results under a new call id (the server re-validates after a revision) are a new
    // compare and a new line, as stock would log it again.
    run.agree("wu1-rev2-check_set", "check_set", 2, None, cases)
        .await;
    assert_eq!(log_lines(&run.quality_dir().join("pairs.jsonl")).len(), 2);
}

#[tokio::test]
async fn a_transient_fetch_failure_is_a_retry_with_no_side_effect_and_the_retry_appends_once() {
    let fx = Fx::new().await;
    let base = quality_base().await;
    let a = named(App::Quality, "a", base.clone());
    let b = named(App::Quality, "b", other_host(&base));
    let run = Run::new(&fx, App::Quality).await;
    // Result 2's output is not there yet: its fetch fails.
    let (status, reply) = run
        .post(
            "wu1-rev1-check_set",
            "check_set",
            2,
            None,
            &[(1, &a), (2, &b)],
            false,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(reply["results"][0], json!({"id": 1, "retry": true}));
    assert_eq!(reply["matches"], json!([]));
    // Nothing recorded, not even for result 1 whose own fetch also failed here, and no work left.
    assert!(
        !run.quality_dir().exists(),
        "{:?}",
        tree(&run.quality_dir())
    );
    // The outputs are there now, but only one of the two: the retry is for 2 alone, and the
    // other result is read but nothing derived from a comparison is recorded.
    fx.output(&a[0].0, &a[0].1);
    fx.output(&a[1].0, &a[1].1);
    fx.output(&a[2].0, &a[2].1);
    let (_, reply) = run
        .post(
            "wu1-rev1-check_set",
            "check_set",
            2,
            None,
            &[(1, &a), (2, &b)],
            false,
        )
        .await;
    assert_eq!(reply["results"][1], json!({"id": 2, "retry": true}));
    assert_eq!(reply["results"][0], json!({"id": 1}));
    assert!(!run.quality_dir().exists());
    assert_eq!(
        std::fs::read_dir(fx.state.path().join("work"))
            .unwrap()
            .count(),
        0
    );
    // Then both are there: the same call id is judged, and its line is added once.
    let (_, reply) = run
        .post(
            "wu1-rev1-check_set",
            "check_set",
            2,
            None,
            &[(1, &a), (2, &b)],
            true,
        )
        .await;
    assert_eq!(matches_of(&reply), BTreeSet::from([(1, 2)]));
    assert_eq!(log_lines(&run.quality_dir().join("pairs.jsonl")).len(), 1);
    let (_, again) = run
        .post(
            "wu1-rev1-check_set",
            "check_set",
            2,
            None,
            &[(1, &a), (2, &b)],
            true,
        )
        .await;
    assert_eq!(again, reply);
    assert_eq!(log_lines(&run.quality_dir().join("pairs.jsonl")).len(), 1);
}

#[tokio::test]
async fn a_late_result_that_cannot_be_fetched_does_not_hold_back_the_others_or_double_their_lines()
{
    let fx = Fx::new().await;
    let base = quality_base().await;
    let a = named(App::Quality, "a", base.clone());
    let ok = named(App::Quality, "d", other_host(&base));
    let slow = named(
        App::Quality,
        "e",
        edited(&base, |r| r.timings.total_ms += 9),
    );
    let run = Run::new(&fx, App::Quality).await;
    entries(&fx, &a, true);
    entries(&fx, &ok, true);
    // check_pair judges each late result on its own (D41): `ok` is compared, `slow` is retry.
    let post_it = |register_slow: bool| {
        let results = vec![
            res(3, entries(&fx, &ok, true)),
            res(4, entries(&fx, &slow, register_slow)),
            res(1, entries(&fx, &a, true)),
        ];
        let b = pbody(
            "wu1-rev2-check_pair",
            App::Quality,
            "check_pair",
            2,
            Some(1),
            results,
        );
        let base = run.base.clone();
        async move { post(&base, "usnm_quality", Some("hook"), &b).await }
    };
    let (_, first) = post_it(false).await;
    assert_eq!(first["results"][1], json!({"id": 4, "retry": true}));
    assert_eq!(matches_of(&first), BTreeSet::from([(1, 3)]));
    assert_eq!(log_lines(&run.quality_dir().join("pairs.jsonl")).len(), 1);
    // The same call again, now complete: result 3 is not logged a second time.
    let (_, second) = post_it(true).await;
    assert_eq!(matches_of(&second), BTreeSet::from([(1, 3), (1, 4)]));
    assert_eq!(log_lines(&run.quality_dir().join("pairs.jsonl")).len(), 2);
}

#[tokio::test]
async fn ndlocr_matches_the_stock_check_and_compare() {
    let fx = Fx::new().await;
    let text = nd_text();
    let a = named(App::Ndlocr, "a", nd_outputs(&text));
    let b = named(App::Ndlocr, "b", nd_outputs(&text.replacen('新', "親", 1)));
    let c = named(App::Ndlocr, "c", nd_outputs("別の記事を読んだ結果"));
    let mut bad = nd_outputs(&text);
    bad[0] = b"tampered".to_vec();
    let bad = named(App::Ndlocr, "bad", bad);
    let run = Run::new(&fx, App::Ndlocr).await;
    // Similar texts agree; a different text does not.
    let r = run
        .agree(
            "wu1-rev1-check_set",
            "check_set",
            2,
            None,
            &[(1, &a), (2, &b), (3, &c)],
        )
        .await;
    assert_eq!(matches_of(&r), BTreeSet::from([(1, 2)]));
    let r = run
        .agree(
            "wu2-rev1-check_set",
            "check_set",
            2,
            None,
            &[(4, &a), (5, &c)],
        )
        .await;
    assert!(matches_of(&r).is_empty());
    // An unreadable result is invalid.
    let r = run
        .agree(
            "wu3-rev1-check_set",
            "check_set",
            2,
            None,
            &[(6, &bad), (7, &a), (8, &b)],
        )
        .await;
    assert_eq!(r["results"][0], json!({"id": 6, "invalid": true}));
    // A late result against the canonical one.
    let r = run
        .agree(
            "wu1-rev2-check_pair",
            "check_pair",
            2,
            Some(1),
            &[(9, &c), (10, &b), (1, &a)],
        )
        .await;
    assert_eq!(matches_of(&r), BTreeSet::from([(1, 10)]));
    // The stock ndlocr scripts leave no file; neither does this.
    assert!(!run.quality_dir().exists());
    let d = named(App::Ndlocr, "d", nd_outputs(&text.replacen('日', "曰", 1)));
    let e = named(App::Ndlocr, "e", nd_outputs(&text.replacen('語', "悟", 1)));
    // A fetch failure is a retry, with a verdict that is not stored: the same id runs again.
    let (_, r) = run
        .post(
            "wu4-rev1-check_set",
            "check_set",
            2,
            None,
            &[(11, &d), (12, &e)],
            false,
        )
        .await;
    assert_eq!(r["results"][0]["retry"], true);
    let fetched = fx.mock.fetches.load(Ordering::SeqCst);
    let r = run
        .agree(
            "wu4-rev1-check_set",
            "check_set",
            2,
            None,
            &[(11, &d), (12, &e)],
        )
        .await;
    assert!(fx.mock.fetches.load(Ordering::SeqCst) > fetched);
    let again = run
        .post(
            "wu4-rev1-check_set",
            "check_set",
            2,
            None,
            &[(11, &d), (12, &e)],
            true,
        )
        .await;
    assert_eq!(again, (200, r));
}

#[tokio::test]
async fn the_file_rules_of_the_init_scripts_hold_for_both_apps() {
    let fx = Fx::new().await;
    let base = quality_base().await;
    let q = named(App::Quality, "a", base);
    let n = named(App::Ndlocr, "a", nd_outputs(&nd_text()));
    for (app, files) in [(App::Quality, &q), (App::Ndlocr, &n)] {
        let run = Run::new(&fx, app).await;
        let good = entries(&fx, files, true);
        let mut big = good.clone();
        // Over the first position's read limit: invalid, with no fetch.
        big[0]["nbytes"] = json!(64 * 1024 * 1024);
        let before = fx.mock.fetches.load(Ordering::SeqCst);
        let results = vec![
            res(1, good[..2].to_vec()),
            res(2, vec![]),
            res(3, big),
            res(4, [good.clone(), good.clone()].concat()),
        ];
        let (s, reply) = post(
            &run.base,
            app.name(),
            Some("hook"),
            &pbody(
                &format!("c-{}", app.name()),
                app,
                "check_set",
                1,
                None,
                results,
            ),
        )
        .await;
        assert_eq!(s, 200, "{reply}");
        for i in 0..4 {
            assert_eq!(reply["results"][i]["invalid"], true, "{reply}");
        }
        assert_eq!(fx.mock.fetches.load(Ordering::SeqCst), before);
        assert_eq!(reply["matches"], json!([]));
    }
}

#[tokio::test]
async fn pairwise_requests_need_a_mode_a_quorum_and_a_canonical_result() {
    let fx = Fx::new().await;
    let run = Run::new(&fx, App::Ndlocr).await;
    let one = || vec![res(1, vec![])];
    let mut b = pbody("c1", App::Ndlocr, "check_set", 2, None, one());
    assert_eq!(
        post(&run.base, "usnm_ndlocr", Some("hook"), &b).await.0,
        200
    );
    b["mode"] = json!("check_both");
    assert_eq!(
        post(&run.base, "usnm_ndlocr", Some("hook"), &b).await.0,
        400
    );
    let b = pbody("c2", App::Ndlocr, "check_set", 0, None, one());
    assert_eq!(
        post(&run.base, "usnm_ndlocr", Some("hook"), &b).await.0,
        400
    );
    let b = pbody("c3", App::Ndlocr, "check_pair", 2, None, one());
    assert_eq!(
        post(&run.base, "usnm_ndlocr", Some("hook"), &b).await.0,
        400
    );
    let b = pbody("c4", App::Ndlocr, "check_pair", 2, Some(9), one());
    assert_eq!(
        post(&run.base, "usnm_ndlocr", Some("hook"), &b).await.0,
        400
    );
    // Wrong app in the path, and the token, as for the gold apps.
    let b = pbody("c5", App::Ndlocr, "check_set", 2, None, one());
    assert_eq!(
        post(&run.base, "usnm_quality", Some("hook"), &b).await.0,
        400
    );
    assert_eq!(post(&run.base, "usnm_ndlocr", None, &b).await.0, 401);
}

#[test]
fn the_pair_journal_adds_a_line_once_even_after_a_crash_between_its_steps() {
    let dir = tempfile::tempdir().unwrap();
    let side = dir.path();
    let log = side.join("pairs.jsonl");
    let (l1, l2, l3) = (
        b"{\"n\":1}\n".to_vec(),
        b"{\"n\":2}\n".to_vec(),
        b"{\"n\":3}\n".to_vec(),
    );
    pairwise_record(side, "k1", &l1);
    pairwise_record(side, "k1", &l1);
    pairwise_record(side, "k2", &l2);
    assert_eq!(
        std::fs::read(&log).unwrap(),
        [l1.clone(), l2.clone()].concat()
    );
    // A crash after the append and before the mark: the line is there, the mark is not.
    std::fs::remove_file(side.join("pairs-journal/k2.done")).unwrap();
    pairwise_record(side, "k2", b"{\"n\":\"a later attempt, another time\"}\n");
    assert_eq!(
        std::fs::read(&log).unwrap(),
        [l1.clone(), l2.clone()].concat()
    );
    // A crash in the middle of the append: only the start of the line is there.
    let torn = [l1.clone(), l2.clone(), l3[..4].to_vec()].concat();
    std::fs::write(&log, &torn).unwrap();
    let journal = side.join("pairs-journal");
    let off = (l1.len() + l2.len()) as u64;
    std::fs::write(
        journal.join("k3.intent"),
        serde_json::to_vec(&json!({"offset": off, "line": String::from_utf8(l3.clone()).unwrap()}))
            .unwrap(),
    )
    .unwrap();
    pairwise_record(side, "k3", &l3);
    assert_eq!(
        std::fs::read(&log).unwrap(),
        [l1.clone(), l2.clone(), l3.clone()].concat()
    );
    // A crash before the append, and another call's line went in meanwhile: neither is lost.
    let l4 = b"{\"n\":4}\n".to_vec();
    std::fs::write(
        journal.join("k4.intent"),
        serde_json::to_vec(&json!({"offset": off, "line": String::from_utf8(l4.clone()).unwrap()}))
            .unwrap(),
    )
    .unwrap();
    pairwise_record(side, "k4", &l4);
    assert_eq!(std::fs::read(&log).unwrap(), [l1, l2, l3, l4].concat());
}

fn pairwise_record(side: &Path, key: &str, line: &[u8]) {
    super::super::pairwise::record_pair(side, key, line).unwrap();
}
