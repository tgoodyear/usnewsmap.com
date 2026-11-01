//! The fetcher against a stand-in server: what `bulk-ocr` did before it moved
//! here has to hold exactly for LoC, and the mirror has to be the same path.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tokio::time::Instant;

use super::engine::{instant_of, ByteCap, Probes};
use super::*;
use crate::bulk::{self, Mirror, Options};
use crate::loc::global::{GlobalPacer, NORMAL_GAP, SLOW_GAP};
use crate::standin::{Scripted, StandIn};

fn sha(b: &[u8]) -> String {
    usnm_core::digest::hex(&Sha256::digest(b))
}

fn test_policy(name: &str, host: &str, extra: &str) -> Policy {
    let rate = if extra.contains("requests_per_minute = 3") {
        ""
    } else {
        "requests_per_minute = 60000"
    };
    let text = format!(
        r#"
hosts = ["{host}"]
label = "{name}"
{rate}
{extra}
[push_back]
default_hold_secs = 1
html = true
max_hold_secs = 3600
statuses = [403, 429]
"#
    );
    let mut p = Policy::parse(name, &text).unwrap();
    p.allow_plain_http = true;
    p
}

/// LoC's rules on the stand-in's host, at test speed.
fn loc_policy() -> Policy {
    test_policy(LOC, "127.0.0.1", "slow_requests_per_minute = 6000\n")
}

/// A mirror on the same server under another name, so it has a policy of its own.
fn mirror_policy(extra: &str) -> Policy {
    test_policy("mirror", "localhost", extra)
}

struct Rig {
    dir: tempfile::TempDir,
    s: StandIn,
    root: String,
    mirror_root: String,
    rows: Vec<serde_json::Value>,
}

impl Rig {
    async fn new() -> Self {
        let s = StandIn::start().await;
        let root = s.iiif_root().trim_end_matches("/iiif/").to_owned();
        let mirror_root = root.replace("127.0.0.1", "localhost");
        Self {
            dir: tempfile::tempdir().unwrap(),
            s,
            root,
            mirror_root,
            rows: Vec::new(),
        }
    }

    fn out(&self) -> PathBuf {
        self.dir.path().join("out")
    }

    fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    fn pacer(&self) -> PathBuf {
        self.dir.path().join("pacer.json")
    }

    /// An archive in LoC's table, served at `/loc/<name>`.
    fn archive(&mut self, name: &str, body: &[u8]) {
        self.listed(name, body, &sha(body));
    }

    fn listed(&mut self, name: &str, body: &[u8], sha256: &str) {
        let name = format!("{name}.tar.bz2");
        self.s.put(&format!("/loc/{name}"), body.to_vec());
        self.rows.push(serde_json::json!({
            "archive_name": name, "batch": name,
            "url": format!("{}/loc/{name}", self.root),
            "size": body.len(), "sha256": sha256,
        }));
    }

    fn write_list(&self) {
        std::fs::create_dir_all(self.out()).unwrap();
        std::fs::write(
            self.out().join("datasets.json"),
            serde_json::to_vec(&serde_json::json!({ "datasets": self.rows })).unwrap(),
        )
        .unwrap();
    }

    fn opts(&self) -> Options {
        Options {
            out: self.out(),
            list_url: format!("{}/list", self.root),
            gap: Duration::ZERO,
            max_hours: 1.0,
            min_free_bytes: 0,
            limit: None,
            global_pacer: self.pacer(),
            policies: Policies::new(vec![loc_policy()]).unwrap(),
            mirror: None,
            state: None,
            workers: 1,
            rate_bytes_per_sec: None,
        }
    }

    /// With a mirror holding `manifest`, served from `/hf/`.
    fn mirrored(&self, manifest: serde_json::Value, policy: Policy, only: bool) -> Options {
        let path = self.dir.path().join("manifest.json");
        std::fs::write(&path, manifest.to_string()).unwrap();
        Options {
            policies: Policies::new(vec![loc_policy(), policy]).unwrap(),
            mirror: Some(Mirror {
                base_url: format!("{}/hf/", self.mirror_root),
                manifest: path,
                only,
            }),
            state: Some(self.state()),
            ..self.opts()
        }
    }

    fn pacer_state(&self) -> crate::loc::global::State {
        GlobalPacer::new(self.pacer()).state().unwrap()
    }

    fn has(&self, name: &str) -> bool {
        self.out().join(format!("{name}.tar.bz2")).exists()
    }
}

fn no_stop() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}

async fn go(o: Options) -> anyhow::Result<Status> {
    let (_tx, rx) = no_stop();
    bulk::run(o, rx).await
}

fn status_of(r: &Rig) -> Status {
    serde_json::from_slice(&std::fs::read(r.out().join("bulk-status.json")).unwrap()).unwrap()
}

fn only_parts(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir).map_or(Vec::new(), |d| {
        d.filter_map(|e| {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            n.ends_with(".part").then_some(n)
        })
        .collect()
    })
}

// ---- policies ----

#[test]
fn locs_policy_keeps_its_limits() {
    let all = Policies::builtin();
    let loc = all.iter().find(|p| p.name == LOC).unwrap();
    assert_eq!(loc.requests_per_minute, 45);
    assert_eq!(loc.slow_requests_per_minute, Some(10));
    assert_eq!(loc.normal_gap(), NORMAL_GAP);
    assert_eq!(loc.slow_gap(), SLOW_GAP);
    assert_eq!(loc.concurrency, 1);
    assert_eq!(loc.min_gap_secs, 60);
    assert_eq!(loc.push_back.statuses, [403, 429]);
    assert!(loc.push_back.html);
    assert!(!loc.resume, "a LoC download restarts, as it always has");
    assert_eq!(loc.attempts, 1, "the next run tries again");
}

#[test]
fn only_https_on_a_policys_hosts() {
    let all = Policies::builtin();
    let name = |u: &str| {
        all.for_url(&reqwest::Url::parse(u).unwrap())
            .map(|p| p.name.clone())
    };
    assert_eq!(
        name("https://chroniclingamerica.loc.gov/data/ocr/x.tar.bz2").unwrap(),
        LOC
    );
    assert_eq!(
        name("https://tile.loc.gov/storage-services/x").unwrap(),
        LOC
    );
    assert_eq!(name("https://loc.gov/x").unwrap(), LOC);
    assert_eq!(
        name("https://us.aws.cdn.hf.co/xet-bridge-us/x").unwrap(),
        "huggingface"
    );
    assert!(name("http://tile.loc.gov/x").is_err());
    assert!(name("https://loc.gov.example.com/x").is_err());
    assert!(name("https://notloc.gov/x").is_err());
    assert!(name("https://example.com/loc.gov").is_err());
    let e = name("https://example.com/x?ticket=hidden").unwrap_err();
    assert!(e.contains("no policy") && !e.contains("hidden"), "{e}");
}

#[test]
fn policies_are_checked_when_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, hosts: &str| {
        std::fs::write(
            dir.path().join(name),
            format!(
                "hosts = {hosts}\nlabel = \"x\"\nrequests_per_minute = 1\n[push_back]\n\
                 default_hold_secs = 1\nhtml = true\nmax_hold_secs = 1\nstatuses = []\n"
            ),
        )
        .unwrap();
    };
    write("a.toml", r#"["a.example"]"#);
    write("b.toml", r#"["*.b.example"]"#);
    std::fs::write(dir.path().join("notes.txt"), "not a policy").unwrap();
    let all = Policies::load_dir(dir.path()).unwrap();
    assert_eq!(all.iter().count(), 2);
    // Two policies can't share a host: which budget would it use?
    write("c.toml", r#"["x.b.example"]"#);
    assert!(Policies::load_dir(dir.path()).is_err());
    write("c.toml", r#"["*.example"]"#);
    assert!(Policies::load_dir(dir.path()).is_err());
    std::fs::remove_file(dir.path().join("c.toml")).unwrap();

    assert!(Policy::parse("x", "hosts = [\"a\"]\nunknown = 1").is_err());
    let bad = |hosts: &str, rate: u32| {
        let t = format!(
            "hosts = {hosts}\nlabel = \"x\"\nrequests_per_minute = {rate}\n[push_back]\n\
             default_hold_secs = 1\nhtml = true\nmax_hold_secs = 1\nstatuses = []\n"
        );
        Policy::parse("x", &t).is_err()
    };
    assert!(bad("[]", 1));
    assert!(bad(r#"["a.example"]"#, 0));
    assert!(bad(r#"["A.example"]"#, 1));
    assert!(bad(r#"["a.*.example"]"#, 1));
    assert!(!bad(r#"["a.example"]"#, 1));
}

// ---- LoC's behaviour ----

/// The file as a deployed pacer has it, slowed, must read as it is, and a run
/// mustn't start, ask anything, or lift the slow rate.
#[tokio::test]
async fn a_deployed_slowed_pacer_file_is_read_and_stays_slowed() {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.write_list();
    let text = r#"{"next_ms":1793494423019,"slowed":{"at":"2026-10-25T20:25:37Z","why":"HTTP 503 (challenge page)"},"sent":15096,"hold_until_ms":1793342672038,"generation":16}"#;
    std::fs::write(r.pacer(), text).unwrap();
    let e = go(r.opts()).await.unwrap_err().to_string();
    assert!(e.contains("HTTP 503 (challenge page)"), "{e}");
    assert_eq!(r.s.hits(".tar.bz2"), 0);
    let st = r.pacer_state();
    assert_eq!(st.sent, 15096);
    assert_eq!(st.generation, 16);
    assert_eq!(st.next_ms, 1_793_494_423_019);
    assert_eq!(st.slowed.unwrap().at, "2026-10-25T20:25:37Z");
}

#[tokio::test]
async fn archives_are_checked_and_named_and_skipped_next_time() {
    let mut r = Rig::new().await;
    r.archive("a", b"first archive");
    r.archive("b", b"second");
    r.write_list();
    let s = go(r.opts()).await.unwrap();
    assert_eq!(s.state, "done");
    assert_eq!((s.done, s.fetched_this_run), (2, 2));
    assert_eq!(s.bytes_this_run, 19);
    assert!(r.has("a") && r.has("b"));
    assert!(only_parts(&r.out()).is_empty());
    assert_eq!(r.pacer_state().sent, 2, "every request is in the budget");
    let s = go(r.opts()).await.unwrap();
    assert_eq!((s.done, s.fetched_this_run), (2, 0));
    assert_eq!(r.s.hits(".tar.bz2"), 2);
}

#[tokio::test]
async fn the_status_file_keeps_its_shape() {
    let mut r = Rig::new().await;
    r.archive("a", b"x");
    r.write_list();
    go(r.opts()).await.unwrap();
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(r.out().join("bulk-status.json")).unwrap()).unwrap();
    let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "archives",
            "bytes_done",
            "bytes_per_sec",
            "bytes_this_run",
            "bytes_total",
            "done",
            "elapsed_secs",
            "failed",
            "fetched_this_run",
            "state",
            "updated_at"
        ]
    );
}

#[tokio::test]
async fn the_list_is_fetched_once_within_the_budget() {
    let mut r = Rig::new().await;
    r.archive("a", b"x");
    r.s.put(
        "/list",
        serde_json::to_vec(&serde_json::json!({ "datasets": r.rows })).unwrap(),
    );
    go(r.opts()).await.unwrap();
    go(r.opts()).await.unwrap();
    assert_eq!(r.s.hits("/list"), 1);
    assert_eq!(r.pacer_state().sent, 2);
}

#[tokio::test]
async fn bad_bytes_never_get_the_final_name() {
    let mut r = Rig::new().await;
    r.listed("a", b"tampered", &"0".repeat(64));
    r.archive("b", b"good");
    r.write_list();
    let s = go(r.opts()).await.unwrap();
    assert_eq!(s.state, "incomplete");
    assert!(!r.has("a") && r.has("b"), "a failure goes on to the next");
    assert!(s.failed[0].contains("sha256"), "{:?}", s.failed);
    assert!(only_parts(&r.out()).is_empty());
}

#[tokio::test]
async fn a_wrong_size_is_refused_before_any_write() {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.rows[0]["size"] = 3.into();
    r.write_list();
    let s = go(r.opts()).await.unwrap();
    assert!(s.failed[0].contains("offers 4 bytes"), "{:?}", s.failed);
    assert!(!r.has("a"));
    assert!(only_parts(&r.out()).is_empty());
}

async fn pushed_back(script: Scripted) -> (Rig, anyhow::Error) {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.archive("b", b"body");
    r.s.script("/loc/a.tar.bz2", script);
    r.write_list();
    let e = go(r.opts()).await.unwrap_err();
    (r, e)
}

#[tokio::test]
async fn a_429_slows_the_budget_holds_and_ends_the_run() {
    let before = crate::loc::global::epoch_ms_in(Duration::from_secs(100));
    let (r, e) = pushed_back(Scripted::RetryAfter(429, 120)).await;
    assert!(e.to_string().contains("HTTP 429"), "{e}");
    assert!(
        e.to_string()
            .contains("the global pacer is at the slow rate"),
        "{e}"
    );
    let st = r.pacer_state();
    assert_eq!(st.slowed.unwrap().why, "bulk OCR: HTTP 429");
    assert!(st.hold_until_ms >= before, "Retry-After is shared");
    assert_eq!(r.s.hits("/loc/b.tar.bz2"), 0, "no next archive");
    assert_eq!(status_of(&r).state, "blocked");
    assert!(!r.has("a"));
}

#[tokio::test]
async fn a_retry_after_hold_is_capped_at_an_hour() {
    let (r, _) = pushed_back(Scripted::RetryAfter(429, 86_400)).await;
    let cap = crate::loc::global::epoch_ms_in(Duration::from_secs(3601));
    assert!(r.pacer_state().hold_until_ms <= cap);
}

#[tokio::test]
async fn a_403_and_a_challenge_page_are_push_back_too() {
    let (r, _) = pushed_back(Scripted::Status(403)).await;
    assert_eq!(r.pacer_state().slowed.unwrap().why, "bulk OCR: HTTP 403");
    let (r, _) = pushed_back(Scripted::Challenge(503)).await;
    assert!(r.pacer_state().slowed.unwrap().why.contains("HTML"));
}

#[tokio::test]
async fn an_html_page_with_a_200_is_push_back() {
    let mut r = Rig::new().await;
    r.archive("a", b"<html>verify you are human</html>");
    r.s.header("/loc/a.tar.bz2", "content-type", "text/html; charset=utf-8");
    r.write_list();
    let e = go(r.opts()).await.unwrap_err().to_string();
    assert!(e.contains("HTTP 200 with an HTML page"), "{e}");
    assert!(r.pacer_state().slowed.is_some());
    assert!(!r.has("a"));
}

#[tokio::test]
async fn a_slow_rate_isnt_lifted_by_any_run_only_by_a_reset() {
    let (r, _) = pushed_back(Scripted::Status(429)).await;
    for _ in 0..2 {
        assert!(go(r.opts()).await.is_err());
        assert!(r.pacer_state().slowed.is_some());
    }
    GlobalPacer::new(r.pacer()).reset().unwrap();
    assert!(go(r.opts()).await.is_ok());
}

#[tokio::test]
async fn a_503_holds_this_process_only() {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.s.script("/loc/a.tar.bz2", Scripted::Status(503));
    r.write_list();
    let s = go(r.opts()).await.unwrap();
    assert_eq!(s.state, "incomplete");
    let st = r.pacer_state();
    assert!(st.slowed.is_none());
    assert_eq!(st.hold_until_ms, 0);
}

#[tokio::test]
async fn a_push_back_to_another_process_stops_the_next_archive() {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.archive("b", b"body");
    r.write_list();
    let mut o = r.opts();
    o.limit = Some(1);
    go(o).await.unwrap();
    GlobalPacer::new(r.pacer()).slow_down("HTTP 429 to the scan fetcher");
    let e = go(r.opts()).await.unwrap_err().to_string();
    assert!(e.contains("the scan fetcher"), "{e}");
    assert_eq!(r.s.hits("/loc/b.tar.bz2"), 0);
}

/// A push-back to another process while a request waits for its turn: it never goes.
#[tokio::test]
async fn a_slow_down_during_the_wait_stops_the_waiting_request() {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.archive("b", b"body");
    r.write_list();
    let mut o = r.opts();
    o.gap = Duration::from_secs(2);
    let run = tokio::spawn(go(o));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(r.s.hits("/loc/a.tar.bz2"), 1);
    GlobalPacer::new(r.pacer()).slow_down("HTTP 429 to the scan fetcher");
    let e = run.await.unwrap().unwrap_err().to_string();
    assert!(e.contains("the scan fetcher"), "{e}");
    assert_eq!(r.s.hits("/loc/b.tar.bz2"), 0);
    assert_eq!(status_of(&r).state, "slowed");
}

/// A mirror can't send the fetcher to another policy's host: that request
/// would be outside the budget it was admitted under.
#[tokio::test]
async fn a_redirect_cant_cross_into_another_policy() {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.s.redirect("/hf/a.tar.bz2", &format!("{}/loc/a.tar.bz2", r.root));
    r.write_list();
    let m = manifest(&[("a", sha(b"body"))]);
    let s = go(r.mirrored(m, mirror_policy(""), true)).await.unwrap();
    assert!(s.failed[0].contains("out of"), "{:?}", s.failed);
    assert_eq!(r.s.hits_under("/loc/"), 0);
    assert!(!r.has("a"));
}

#[tokio::test]
async fn redirects_are_paced_and_held_to_the_policy() {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.s.put("/moved/a", b"body".to_vec());
    r.rows[0]["url"] = format!("{}/start/a", r.root).into();
    r.s.redirect("/start/a", "/moved/a");
    r.write_list();
    let mut o = r.opts();
    o.gap = Duration::from_secs(1);
    let s = go(o).await.unwrap();
    assert_eq!(s.fetched_this_run, 1);
    let (a, b) = (r.s.times("/start/a")[0], r.s.times("/moved/a")[0]);
    assert!(b - a >= Duration::from_millis(950), "{:?}", b - a);
    assert_eq!(r.pacer_state().sent, 2, "each hop is a slot in the budget");
}

/// A redirect to a host with no policy is refused and nothing is asked of it,
/// as is a job on such a host.
#[tokio::test]
async fn hosts_without_a_policy_are_refused() {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.rows[0]["url"] = format!("{}/start/a", r.root).into();
    r.s.redirect("/start/a", "http://unlisted.invalid/a");
    r.archive("b", b"body");
    r.rows[1]["url"] = "https://unlisted.invalid/b".into();
    r.write_list();
    let s = go(r.opts()).await.unwrap();
    assert_eq!(s.fetched_this_run, 0);
    assert_eq!(s.failed.len(), 2, "{:?}", s.failed);
    assert!(
        s.failed.iter().all(|f| f.contains("no policy")),
        "{:?}",
        s.failed
    );
    assert_eq!(r.pacer_state().sent, 1);
    assert!(r.pacer_state().slowed.is_none());
}

#[tokio::test]
async fn a_gap_outlives_a_restart() {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.archive("b", b"body");
    r.write_list();
    let mut o = r.opts();
    o.gap = Duration::from_secs(1);
    o.limit = Some(1);
    go(o.clone()).await.unwrap();
    o.limit = None;
    go(o).await.unwrap();
    let (a, b) = (
        r.s.times("/loc/a.tar.bz2")[0],
        r.s.times("/loc/b.tar.bz2")[0],
    );
    assert!(b - a >= Duration::from_millis(950), "{:?}", b - a);
}

#[test]
fn a_stamp_from_the_future_is_now() {
    let now = crate::loc::global::epoch_ms_in(Duration::ZERO);
    let ago = instant_of(now - 30_000).unwrap().elapsed();
    assert!(
        ago >= Duration::from_secs(29) && ago <= Duration::from_secs(31),
        "{ago:?}"
    );
    let ahead = instant_of(now + 600_000).unwrap().elapsed();
    assert!(ahead < Duration::from_secs(1));
}

#[test]
fn a_garbled_stamp_is_no_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let stamp = dir.path().join("s");
    let g = GlobalPacer::new(dir.path().join("p")).with_stamp(stamp.clone());
    assert!(g.read_stamp().is_none());
    std::fs::write(&stamp, "garbage").unwrap();
    assert!(g.read_stamp().is_none());
    g.write_stamp();
    assert!(g.read_stamp().is_some());
}

#[tokio::test]
async fn a_second_run_is_refused_while_one_holds_the_lock() {
    let mut r = Rig::new().await;
    r.archive("a", b"x");
    r.write_list();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(r.out().join(".bulk.lock"))
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
    let e = go(r.opts()).await.unwrap_err().to_string();
    assert!(e.contains("holds"), "{e}");
    assert_eq!(r.s.hits(".tar.bz2"), 0);
}

#[tokio::test]
async fn the_disk_floor_stops_the_run() {
    let mut r = Rig::new().await;
    r.archive("a", b"x");
    r.write_list();
    let mut o = r.opts();
    o.min_free_bytes = u64::MAX / 2;
    let e = go(o).await.unwrap_err().to_string();
    assert!(e.contains("must stay free"), "{e}");
    assert_eq!(status_of(&r).state, "disk");
    assert_eq!(r.s.hits(".tar.bz2"), 0);
}

#[tokio::test]
async fn max_hours_stops_starting_archives() {
    let mut r = Rig::new().await;
    r.archive("a", b"x");
    r.write_list();
    let mut o = r.opts();
    o.max_hours = 0.0;
    assert_eq!(go(o).await.unwrap().state, "paused");
    assert_eq!(r.s.hits(".tar.bz2"), 0);
}

#[tokio::test]
async fn a_stop_signal_ends_the_run_before_the_next_request() {
    let mut r = Rig::new().await;
    r.archive("a", b"x");
    r.write_list();
    let (tx, rx) = no_stop();
    tx.send_replace(true);
    assert_eq!(bulk::run(r.opts(), rx).await.unwrap().state, "stopped");
    assert_eq!(r.s.hits(".tar.bz2"), 0);
}

#[tokio::test]
async fn a_stop_during_a_wait_ends_it_too() {
    let mut r = Rig::new().await;
    r.archive("a", b"x");
    r.archive("b", b"x");
    r.write_list();
    let mut o = r.opts();
    o.gap = Duration::from_secs(30);
    let (tx, rx) = no_stop();
    let run = tokio::spawn(bulk::run(o, rx));
    tokio::time::sleep(Duration::from_millis(300)).await;
    tx.send_replace(true);
    let s = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(s.state, "stopped");
    assert_eq!(r.s.hits(".tar.bz2"), 1);
}

#[tokio::test]
async fn the_limit_counts_archives_fetched() {
    let mut r = Rig::new().await;
    r.archive("a", b"x");
    r.archive("b", b"x");
    r.write_list();
    let mut o = r.opts();
    o.limit = Some(1);
    assert_eq!(go(o).await.unwrap().state, "limit");
    assert!(r.has("a") && !r.has("b"));
}

#[tokio::test]
async fn a_partial_from_an_earlier_loc_run_is_not_resumed() {
    let mut r = Rig::new().await;
    r.archive("a", b"whole body");
    r.write_list();
    std::fs::write(r.out().join("a.tar.bz2.part"), b"whole").unwrap();
    go(r.opts()).await.unwrap();
    assert_eq!(
        std::fs::read(r.out().join("a.tar.bz2")).unwrap(),
        b"whole body"
    );
}

#[tokio::test]
async fn a_wrong_sized_file_is_replaced_and_a_good_one_never() {
    let mut r = Rig::new().await;
    r.archive("a", b"whole body");
    r.write_list();
    std::fs::write(r.out().join("a.tar.bz2"), b"short").unwrap();
    go(r.opts()).await.unwrap();
    assert_eq!(
        std::fs::read(r.out().join("a.tar.bz2")).unwrap(),
        b"whole body"
    );
    let job = Job {
        name: "a".into(),
        size: 10,
        sha256: String::new(),
        dest: r.out().join("a.tar.bz2"),
        sources: Vec::new(),
    };
    let part = r.out().join("late.part");
    std::fs::write(&part, b"other body").unwrap();
    assert!(!super::engine::publish(&part, &job).unwrap());
    assert!(!part.exists());
    assert_eq!(std::fs::read(&job.dest).unwrap(), b"whole body");
}

// ---- the mirror ----

fn manifest(rows: &[(&str, String)]) -> serde_json::Value {
    let rows: Vec<_> = rows
        .iter()
        .map(|(n, s)| serde_json::json!({"archive_name": format!("{n}.tar.bz2"), "sha256": s}))
        .collect();
    serde_json::json!({"meta": {"generated": "x"}, "files": {"rows": rows}})
}

#[tokio::test]
async fn the_mirror_comes_first_and_loc_covers_what_it_cant_supply() {
    let mut r = Rig::new().await;
    r.archive("a", b"on the mirror");
    r.archive("b", b"not on the mirror");
    r.archive("c", b"differs on the mirror");
    r.s.put("/hf/a.tar.bz2", b"on the mirror".to_vec());
    r.write_list();
    let m = manifest(&[("a", sha(b"on the mirror")), ("c", sha(b"something else"))]);
    let s = go(r.mirrored(m, mirror_policy(""), false)).await.unwrap();
    assert_eq!(s.state, "done");
    assert_eq!(s.held_back, 2);
    assert_eq!(
        (r.s.hits("/hf/a.tar.bz2"), r.s.hits("/loc/a.tar.bz2")),
        (1, 0)
    );
    assert_eq!(r.s.hits("/loc/b.tar.bz2"), 1);
    assert_eq!(
        r.s.hits("/hf/c.tar.bz2"),
        0,
        "a differing mirror is never asked"
    );
    assert_eq!(r.s.hits("/loc/c.tar.bz2"), 1);
    let held = std::fs::read_to_string(r.state().join("held-back.tsv")).unwrap();
    let reasons: Vec<_> = held
        .lines()
        .map(|l| l.split('\t').next().unwrap())
        .collect();
    assert_eq!(reasons, ["absent-from-mirror", "sha256-differs"]);
    assert_eq!(held.lines().next().unwrap().split('\t').count(), 5);
    assert_eq!(r.pacer_state().sent, 2, "LoC's budget counts only LoC");
}

#[tokio::test]
async fn a_mirror_only_run_never_touches_loc_or_its_budget() {
    let mut r = Rig::new().await;
    r.archive("a", b"on the mirror");
    r.archive("b", b"not on the mirror");
    r.s.put("/hf/a.tar.bz2", b"on the mirror".to_vec());
    r.write_list();
    GlobalPacer::new(r.pacer()).slow_down("HTTP 503 (challenge page)");
    let st = r.pacer_state();
    let m = manifest(&[("a", sha(b"on the mirror"))]);
    let s = go(r.mirrored(m, mirror_policy(""), true)).await.unwrap();
    assert_eq!(s.state, "incomplete");
    assert_eq!(s.fetched_this_run, 1);
    assert_eq!(r.s.hits_under("/loc/"), 0);
    assert_eq!(r.pacer_state(), st);
}

#[tokio::test]
async fn a_mirror_only_run_wont_fetch_the_list_from_loc() {
    let r = Rig::new().await;
    let m = manifest(&[("a", sha(b"x"))]);
    let e = go(r.mirrored(m, mirror_policy(""), true))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("never asks LoC"), "{e}");
    assert_eq!(r.s.hits("/list"), 0);
}

/// LoC being slowed stops only what needs LoC.
#[tokio::test]
async fn a_slowed_loc_stops_only_what_needs_loc() {
    let mut r = Rig::new().await;
    r.archive("a", b"not on the mirror");
    r.archive("b", b"on the mirror");
    r.s.put("/hf/b.tar.bz2", b"on the mirror".to_vec());
    r.write_list();
    GlobalPacer::new(r.pacer()).slow_down("HTTP 429");
    let m = manifest(&[("b", sha(b"on the mirror"))]);
    let e = go(r.mirrored(m, mirror_policy(""), false))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("slowed"), "{e}");
    assert!(r.has("b") && !r.has("a"));
    assert_eq!(r.s.hits_under("/loc/"), 0);
    assert_eq!(status_of(&r).state, "slowed");
}

#[tokio::test]
async fn a_mirror_push_back_falls_to_loc_and_leaves_locs_budget_alone() {
    let mut r = Rig::new().await;
    r.archive("a", b"body a");
    r.archive("b", b"body b");
    r.s.put("/hf/a.tar.bz2", b"body a".to_vec());
    r.s.put("/hf/b.tar.bz2", b"body b".to_vec());
    r.s.script("/hf/a.tar.bz2", Scripted::Status(429));
    r.write_list();
    let m = manifest(&[("a", sha(b"body a")), ("b", sha(b"body b"))]);
    let e = go(r.mirrored(m, mirror_policy(""), false))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("nothing more"), "{e}");
    assert!(r.has("a") && r.has("b"));
    assert_eq!(r.s.hits("/hf/b.tar.bz2"), 0, "nothing more to the mirror");
    assert!(r.pacer_state().slowed.is_none());
    assert_eq!(status_of(&r).state, "blocked");
    // No slow rate in its policy, so nothing lasting in its budget.
    let budget = GlobalPacer::new(r.state().join("budgets/mirror.json"));
    assert!(budget.state().unwrap().slowed.is_none());
    assert!(budget.state().unwrap().hold_until_ms > 0);
}

#[tokio::test]
async fn a_mirror_failure_tries_the_next_source() {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.write_list();
    let m = manifest(&[("a", sha(b"body"))]);
    let s = go(r.mirrored(m, mirror_policy(""), false)).await.unwrap();
    assert!(r.has("a"));
    assert_eq!(s.state, "done");
    assert_eq!(s.failed.len(), 1, "the mirror's 404: {:?}", s.failed);
}

#[tokio::test]
async fn checksum_failures_are_kept_and_stop_the_mirror() {
    let mut r = Rig::new().await;
    for n in ["a", "b", "c", "d"] {
        let want = format!("real {n}");
        r.archive(n, want.as_bytes());
        r.s.put(
            &format!("/hf/{n}.tar.bz2"),
            format!("fake {n}").into_bytes(),
        );
    }
    r.write_list();
    let m = manifest(&["a", "b", "c", "d"].map(|n| (n, sha(format!("real {n}").as_bytes()))));
    let o = r.mirrored(m, mirror_policy(""), true);
    let e = go(o.clone()).await.unwrap_err().to_string();
    assert!(e.contains("3 verification failures"), "{e}");
    assert_eq!(status_of(&r).state, "checksum");
    assert_eq!(r.s.hits("/hf/d.tar.bz2"), 0);
    let tsv = std::fs::read_to_string(r.state().join("checksum-failures.tsv")).unwrap();
    assert_eq!(tsv.lines().count(), 3);
    assert_eq!(tsv.lines().next().unwrap().split('\t').count(), 3);
    assert!(tsv.contains("sha256 "), "{tsv}");
    assert!(only_parts(&r.state().join("part")).is_empty());
    // They count across runs: a restart refuses rather than try again.
    let e = go(o).await.unwrap_err().to_string();
    assert!(e.contains("refusing to start"), "{e}");
    assert_eq!(r.s.hits("/hf/d.tar.bz2"), 0);
}

#[tokio::test]
async fn transient_failures_retry_up_to_the_policys_attempts() {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.s.put("/hf/a.tar.bz2", b"body".to_vec());
    r.s.script("/hf/a.tar.bz2", Scripted::Status(500));
    r.s.script("/hf/a.tar.bz2", Scripted::Status(500));
    r.write_list();
    let m = manifest(&[("a", sha(b"body"))]);
    let s = go(r.mirrored(m, mirror_policy("attempts = 3\n"), true))
        .await
        .unwrap();
    assert_eq!(s.state, "done");
    assert_eq!(r.s.hits("/hf/a.tar.bz2"), 3);
}

#[tokio::test]
async fn too_many_failures_in_a_row_end_the_mirror() {
    let mut r = Rig::new().await;
    for n in ["a", "b", "c"] {
        r.archive(n, b"body");
        r.s.put(&format!("/hf/{n}.tar.bz2"), b"body".to_vec());
        r.s.script(&format!("/hf/{n}.tar.bz2"), Scripted::Status(500));
    }
    r.write_list();
    let m = manifest(&["a", "b", "c"].map(|n| (n, sha(b"body"))));
    let policy = mirror_policy("max_failures_in_a_row = 2\n");
    let e = go(r.mirrored(m, policy, true)).await.unwrap_err();
    assert!(e.to_string().contains("2 transient failures"), "{e}");
    assert_eq!(r.s.hits("/hf/c.tar.bz2"), 0);
}

#[tokio::test]
async fn concurrency_is_the_policys_whatever_the_workers() {
    let mut r = Rig::new().await;
    let names = ["a", "b", "c", "d", "e", "f"];
    for n in names {
        r.archive(n, b"body");
        r.s.put(&format!("/hf/{n}.tar.bz2"), b"body".to_vec());
    }
    r.write_list();
    r.s.delay(Duration::from_millis(100));
    let m = manifest(&names.map(|n| (n, sha(b"body"))));
    let mut o = r.mirrored(m, mirror_policy("concurrency = 2\n"), true);
    o.workers = 4;
    let s = go(o).await.unwrap();
    assert_eq!(s.fetched_this_run, 6);
    assert_eq!(r.s.most_at_once(), 2);
    // LoC's concurrency of 1 holds with workers to spare.
    let mut r = Rig::new().await;
    for n in names {
        r.archive(n, b"body");
    }
    r.write_list();
    r.s.delay(Duration::from_millis(50));
    let mut o = r.opts();
    o.workers = 4;
    assert_eq!(go(o).await.unwrap().fetched_this_run, 6);
    assert_eq!(r.s.most_at_once(), 1);
}

/// A server that answers `Range`, as the mirror's CDN does, and notes what it was asked.
async fn range_server(body: &'static [u8]) -> (String, Arc<std::sync::Mutex<Vec<Option<String>>>>) {
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&asked);
    let app = axum::Router::new().fallback(move |headers: HeaderMap| {
        let seen = Arc::clone(&seen);
        async move {
            let range = headers
                .get(header::RANGE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            seen.lock().unwrap().push(range.clone());
            let Some(from) = range.and_then(|r| {
                r.strip_prefix("bytes=")?
                    .strip_suffix('-')?
                    .parse::<usize>()
                    .ok()
            }) else {
                return body.into_response();
            };
            if from >= body.len() {
                return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            }
            (
                StatusCode::PARTIAL_CONTENT,
                [(
                    header::CONTENT_RANGE,
                    format!("bytes {from}-{}/{}", body.len() - 1, body.len()),
                )],
                &body[from..],
            )
                .into_response()
        }
    });
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://localhost:{}/hf/", l.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (root, asked)
}

async fn resume_rig(body: &'static [u8], part: &[u8]) -> (Rig, Options) {
    let mut r = Rig::new().await;
    r.archive("a", body);
    r.write_list();
    let (root, _) = range_server(body).await;
    let m = manifest(&[("a", sha(body))]);
    let mut o = r.mirrored(m, mirror_policy("resume = true\n"), true);
    o.mirror.as_mut().unwrap().base_url = root;
    std::fs::create_dir_all(r.state().join("part")).unwrap();
    std::fs::write(r.state().join("part/a.tar.bz2.part"), part).unwrap();
    (r, o)
}

#[tokio::test]
async fn a_mirror_download_resumes_and_verifies_the_whole_file() {
    let (r, o) = resume_rig(b"0123456789", b"01234").await;
    let s = go(o).await.unwrap();
    assert_eq!(s.bytes_this_run, 5);
    assert_eq!(s.bytes_done, 10);
    assert_eq!(
        std::fs::read(r.out().join("a.tar.bz2")).unwrap(),
        b"0123456789"
    );
    assert!(only_parts(&r.state().join("part")).is_empty());
}

#[tokio::test]
async fn a_finished_part_needs_no_request() {
    let (r, o) = resume_rig(b"0123456789", b"0123456789").await;
    let s = go(o).await.unwrap();
    assert_eq!(s.bytes_this_run, 0);
    assert!(r.has("a"));
}

#[tokio::test]
async fn a_part_with_wrong_bytes_fails_verification_and_goes() {
    let (r, o) = resume_rig(b"0123456789", b"XXXXX").await;
    let s = go(o).await.unwrap();
    assert!(!r.has("a"));
    assert_eq!(s.checksum_failures, 1);
    assert!(only_parts(&r.state().join("part")).is_empty());
}

#[tokio::test]
async fn a_part_longer_than_the_file_starts_over() {
    let (r, o) = resume_rig(b"0123456789", b"0123456789 and more").await;
    go(o).await.unwrap();
    assert_eq!(
        std::fs::read(r.out().join("a.tar.bz2")).unwrap(),
        b"0123456789"
    );
}

/// A server that ignores `Range` sends the whole file; the part starts over.
#[tokio::test]
async fn a_whole_file_answer_replaces_the_part() {
    let mut r = Rig::new().await;
    r.archive("a", b"0123456789");
    r.s.put("/hf/a.tar.bz2", b"0123456789".to_vec());
    r.write_list();
    let m = manifest(&[("a", sha(b"0123456789"))]);
    let o = r.mirrored(m, mirror_policy("resume = true\n"), true);
    std::fs::create_dir_all(r.state().join("part")).unwrap();
    std::fs::write(r.state().join("part/a.tar.bz2.part"), b"XXXXX").unwrap();
    go(o).await.unwrap();
    assert_eq!(
        std::fs::read(r.out().join("a.tar.bz2")).unwrap(),
        b"0123456789"
    );
}

/// A CDN redirect's query is a signature: it stays out of what's recorded.
#[tokio::test]
async fn a_failure_after_a_signed_redirect_shows_no_signature() {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.s.redirect("/hf/a.tar.bz2", "/cdn/a?Signature=secret");
    r.write_list();
    let m = manifest(&[("a", sha(b"body"))]);
    let s = go(r.mirrored(m, mirror_policy(""), true)).await.unwrap();
    assert_eq!(s.failed.len(), 1, "{:?}", s.failed);
    assert!(s.failed[0].contains("/cdn/a"), "{:?}", s.failed);
    assert!(!s.failed[0].contains("secret"), "{:?}", s.failed);
}

#[tokio::test(start_paused = true)]
async fn the_byte_cap_is_shared_and_keeps_to_its_rate() {
    let cap = ByteCap::new(Some(1_000_000), None);
    let t = Instant::now();
    // The second take starts where the first's second of bytes ends.
    cap.take(1_000_000).await;
    cap.take(1_000_000).await;
    cap.take(1).await;
    assert!(t.elapsed() >= Duration::from_secs(2), "{:?}", t.elapsed());
    assert!(
        t.elapsed() < Duration::from_millis(2100),
        "{:?}",
        t.elapsed()
    );
}

#[test]
fn a_configured_cap_under_the_floor_stands() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("rate-bytes-per-sec");
    std::fs::write(&f, "5").unwrap();
    assert_eq!(
        ByteCap::new(Some(500_000), Some(f)).current(),
        Some(500_000)
    );
}

#[tokio::test]
async fn a_byte_cap_of_zero_is_refused_not_taken_as_none() {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.write_list();
    let m = manifest(&[("a", sha(b"body"))]);
    let mut o = r.mirrored(m, mirror_policy(""), true);
    o.rate_bytes_per_sec = Some(0);
    let e = go(o).await.unwrap_err().to_string();
    assert!(e.contains("byte cap of zero"), "{e}");
    assert_eq!(r.s.hits_under("/hf/"), 0);
    let text = "hosts = [\"a.example\"]\nlabel = \"x\"\nrequests_per_minute = 1\n\
                [push_back]\ndefault_hold_secs = 1\nhtml = true\nmax_hold_secs = 1\n\
                statuses = []\n[serving_probe]\nevery_secs = 1\nfloor_bytes_per_sec = 0\n\
                slow_secs = 1.0\nurl = \"https://a.example/\"\n";
    assert!(Policy::parse("x", text).is_err());
}

#[test]
fn a_rate_file_only_lowers_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("rate-bytes-per-sec");
    let cap = ByteCap::new(Some(85_000_000), Some(f.clone()));
    assert_eq!(cap.current(), Some(85_000_000));
    std::fs::write(&f, "20000000\n").unwrap();
    cap.refresh();
    assert_eq!(cap.current(), Some(20_000_000));
    std::fs::write(&f, "900000000").unwrap();
    cap.refresh();
    assert_eq!(cap.current(), Some(85_000_000));
    std::fs::write(&f, "5").unwrap();
    cap.refresh();
    assert_eq!(cap.current(), Some(1_000_000), "the floor");
    std::fs::write(&f, "nonsense").unwrap();
    cap.refresh();
    assert_eq!(cap.current(), Some(85_000_000));
    assert_eq!(ByteCap::new(None, Some(f)).current(), None);
}

#[test]
fn a_slow_median_lowers_and_a_fast_one_doesnt() {
    let mut p = Probes::default();
    for t in [0.1, 0.1, 5.0, 5.0] {
        assert!(!p.slow_after(t, 1.0));
    }
    assert!(!p.slow_after(0.2, 1.0), "the median is 0.2");
    assert!(p.slow_after(5.0, 1.0), "the last five's median is 5");
    for _ in 0..4 {
        assert!(!p.slow_after(5.0, 1.0), "it starts counting again");
    }
    assert!(p.slow_after(5.0, 1.0));
}

#[test]
fn a_manifest_is_found_wherever_its_rows_are() {
    let m = bulk::parse_manifest(&manifest(&[("a", "ABC".into())])).unwrap();
    assert_eq!(m["a.tar.bz2"], "abc");
    let flat = serde_json::json!([{"archive_name": "x", "sha256": "1"}]);
    assert_eq!(bulk::parse_manifest(&flat).unwrap().len(), 1);
    assert!(bulk::parse_manifest(&serde_json::json!({"rows": []})).is_none());
}

/// One job failing mustn't lose the job another worker was holding back.
#[tokio::test]
async fn a_failure_frees_the_limit_for_a_job_held_back() {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.archive("b", b"body");
    r.s.put("/hf/b.tar.bz2", b"body".to_vec());
    r.s.delay(Duration::from_millis(100));
    r.write_list();
    let m = manifest(&[("a", sha(b"body")), ("b", sha(b"body"))]);
    let mut o = r.mirrored(m, mirror_policy("concurrency = 2\n"), true);
    o.workers = 2;
    o.limit = Some(1);
    let s = go(o).await.unwrap();
    assert_eq!(s.fetched_this_run, 1);
    assert!(r.has("b"), "{:?}", s.failed);
}

#[tokio::test]
async fn two_runs_cant_share_a_state_directory() {
    let mut r = Rig::new().await;
    r.archive("a", b"body");
    r.write_list();
    let m = manifest(&[("a", sha(b"body"))]);
    let o = r.mirrored(m, mirror_policy(""), true);
    std::fs::create_dir_all(r.state()).unwrap();
    let held = std::fs::File::create(r.state().join(".state.lock")).unwrap();
    rustix::fs::flock(&held, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
    let e = go(o).await.unwrap_err().to_string();
    assert!(e.contains(".state.lock"), "{e}");
    assert_eq!(r.s.hits_under("/hf/"), 0);
}

#[test]
fn a_reservation_follows_the_bytes_still_to_come() {
    let total = std::sync::atomic::AtomicU64::new(100);
    {
        let room = Reservation::new(&total);
        assert_eq!(room.set(50), 100, "what others hold");
        assert_eq!(total.load(Ordering::SeqCst), 150);
        room.set(10);
        assert_eq!(total.load(Ordering::SeqCst), 110);
        room.set(50);
        assert_eq!(total.load(Ordering::SeqCst), 150, "a restart takes it back");
    }
    assert_eq!(total.load(Ordering::SeqCst), 100);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failure_records_stay_whole_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileJobs::new(
        Vec::new(),
        dir.path().join("s.json"),
        Some(dir.path().to_owned()),
    ));
    let mut set = tokio::task::JoinSet::new();
    for i in 0..64 {
        let store = Arc::clone(&store);
        set.spawn(async move {
            let job = Job {
                name: format!("n{i}"),
                size: 0,
                sha256: String::new(),
                dest: PathBuf::new(),
                sources: Vec::new(),
            };
            store
                .checksum_failed(&job, &"x".repeat(4096))
                .await
                .unwrap()
        });
    }
    let mut counts = Vec::new();
    while let Some(n) = set.join_next().await {
        counts.push(n.unwrap());
    }
    counts.sort_unstable();
    assert_eq!(counts, (1..=64).collect::<Vec<u64>>());
    let text = std::fs::read_to_string(dir.path().join("checksum-failures.tsv")).unwrap();
    assert!(text.lines().all(|l| l.split('\t').count() == 3));
}

/// A low byte cap can mean a long wait, and a stop has to end it.
#[tokio::test]
async fn a_stop_ends_a_throttled_wait_and_keeps_the_part() {
    let mut r = Rig::new().await;
    let body = vec![7u8; 3_000_000];
    r.archive("a", &body);
    r.s.put("/hf/a.tar.bz2", body.clone());
    r.write_list();
    let m = manifest(&[("a", sha(&body))]);
    let mut o = r.mirrored(
        m,
        mirror_policy("max_bytes_per_sec = 1000\nresume = true\n"),
        true,
    );
    o.workers = 1;
    let (tx, rx) = no_stop();
    let run = tokio::spawn(bulk::run(o, rx));
    tokio::time::sleep(Duration::from_millis(300)).await;
    tx.send_replace(true);
    let s = tokio::time::timeout(Duration::from_secs(3), run)
        .await
        .expect("a stop doesn't wait for the throttle")
        .unwrap()
        .unwrap();
    assert_eq!(s.state, "stopped");
    assert_eq!(only_parts(&r.state().join("part")).len(), 1);
}

#[tokio::test]
async fn the_limit_holds_with_workers_racing_for_it() {
    let mut r = Rig::new().await;
    let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
    for n in names {
        r.archive(n, b"body");
        r.s.put(&format!("/hf/{n}.tar.bz2"), b"body".to_vec());
    }
    r.write_list();
    r.s.delay(Duration::from_millis(50));
    let m = manifest(&names.map(|n| (n, sha(b"body"))));
    let mut o = r.mirrored(m, mirror_policy("concurrency = 8\n"), true);
    o.workers = 8;
    o.limit = Some(1);
    let s = go(o).await.unwrap();
    assert_eq!(s.fetched_this_run, 1);
    assert_eq!(r.s.hits_under("/hf/"), 1);
}

/// Workers finish together, and the status file has to stay whole throughout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_writes_from_many_workers_stay_whole() {
    let mut r = Rig::new().await;
    let names: Vec<String> = (0..60).map(|i| format!("n{i}")).collect();
    for n in &names {
        r.archive(n, b"body");
        r.s.put(&format!("/hf/{n}.tar.bz2"), b"body".to_vec());
    }
    r.write_list();
    let rows: Vec<(&str, String)> = names.iter().map(|n| (n.as_str(), sha(b"body"))).collect();
    let mut o = r.mirrored(manifest(&rows), mirror_policy("concurrency = 8\n"), true);
    o.workers = 8;
    let status = r.out().join("bulk-status.json");
    let run = tokio::spawn(go(o));
    while !run.is_finished() {
        if let Ok(b) = std::fs::read(&status) {
            assert!(serde_json::from_slice::<Status>(&b).is_ok(), "torn status");
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(run.await.unwrap().unwrap().fetched_this_run, 60);
    assert!(!r.out().join("bulk-status.json.tmp").exists());
}

/// A partial's name comes from a hard link, so the run says so before it downloads.
#[tokio::test]
async fn parts_off_the_destinations_filesystem_are_refused_up_front() {
    if !Path::new("/dev/shm").is_dir() {
        return;
    }
    let shm = tempfile::tempdir_in("/dev/shm").unwrap();
    let r = Rig::new().await;
    std::fs::create_dir_all(r.out()).unwrap();
    let (_tx, rx) = no_stop();
    let engine = Engine::new(
        Policies::new(vec![loc_policy()]).unwrap(),
        Box::new(FileBudgets::new(None)),
        EngineOptions {
            who: "test".into(),
            rate_file: None,
        },
    )
    .unwrap();
    let e = run::run(
        Arc::new(engine),
        Arc::new(FileJobs::new(Vec::new(), r.out().join("s.json"), None)),
        run::RunOptions {
            part_dir: shm.path().to_owned(),
            out: r.out(),
            workers: 1,
            run_for: Duration::from_secs(60),
            min_free_bytes: 0,
            limit: None,
            max_checksum_failures: None,
        },
        Status::default(),
        rx,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(e.contains("one filesystem"), "{e}");
}

/// A 429 on the mirror holds its budget for an hour. Jobs already queued for
/// that host mustn't wait it out and then ask anyway: they go to LoC now.
async fn closed_while_waiting(extra: &str) {
    let mut r = Rig::new().await;
    r.archive("a", b"body a");
    r.archive("b", b"body b");
    r.s.put("/hf/a.tar.bz2", b"body a".to_vec());
    r.s.put("/hf/b.tar.bz2", b"body b".to_vec());
    // Either worker can get there first.
    r.s.script("/hf/a.tar.bz2", Scripted::RetryAfter(429, 3600));
    r.s.script("/hf/b.tar.bz2", Scripted::RetryAfter(429, 3600));
    r.write_list();
    let m = manifest(&[("a", sha(b"body a")), ("b", sha(b"body b"))]);
    let mut o = r.mirrored(m, mirror_policy(extra), false);
    o.workers = 2;
    let e = tokio::time::timeout(Duration::from_secs(10), go(o))
        .await
        .expect("a closed host's hold isn't waited out")
        .unwrap_err();
    assert!(e.to_string().contains("nothing more"), "{e}");
    assert_eq!(r.s.hits_under("/hf/"), 1, "one 429 was enough");
    assert!(r.has("a") && r.has("b"), "both came from LoC");
}

#[tokio::test]
async fn a_closed_host_gets_nothing_from_requests_waiting_for_their_turn() {
    closed_while_waiting("concurrency = 2\nrequests_per_minute = 300\n".trim_end()).await;
}

#[tokio::test]
async fn a_closed_host_gets_nothing_from_jobs_waiting_for_a_slot() {
    closed_while_waiting("concurrency = 1\n").await;
}

#[tokio::test]
async fn budgets_are_a_seam_any_implementation_can_fill() {
    /// A budget in memory, standing in for one that isn't a file.
    #[derive(Debug, Default)]
    struct Counted {
        sent: std::sync::atomic::AtomicU64,
    }
    #[async_trait::async_trait]
    impl Budget for Counted {
        async fn reserve(&self, _not_before_ms: u64) -> std::io::Result<(u64, u64)> {
            self.sent.fetch_add(1, Ordering::SeqCst);
            Ok((0, 0))
        }
        async fn still_good(&self, _generation: u64) -> bool {
            true
        }
        async fn hold(&self, _d: Duration) {}
        async fn slow_down(&self, _why: &str) {}
        async fn state(&self) -> std::io::Result<crate::loc::global::State> {
            Ok(crate::loc::global::State::default())
        }
        async fn reset(&self) -> std::io::Result<()> {
            Ok(())
        }
        fn retry_gap(&self) -> Duration {
            Duration::from_millis(1)
        }
    }
    struct One(Arc<Counted>);
    impl Budgets for One {
        fn budget(&self, _policy: &Policy) -> anyhow::Result<Arc<dyn Budget>> {
            Ok(Arc::clone(&self.0) as Arc<dyn Budget>)
        }
    }
    let r = Rig::new().await;
    r.s.put("/f", b"body".to_vec());
    std::fs::create_dir_all(r.out()).unwrap();
    let counted = Arc::new(Counted::default());
    let engine = Engine::new(
        Policies::new(vec![loc_policy()]).unwrap(),
        Box::new(One(Arc::clone(&counted))),
        EngineOptions {
            who: "test".into(),
            rate_file: None,
        },
    )
    .unwrap();
    let job = Job {
        name: "f".into(),
        size: 4,
        sha256: sha(b"body"),
        dest: r.out().join("f"),
        sources: Vec::new(),
    };
    let (_tx, rx) = no_stop();
    let far = Instant::now() + Duration::from_secs(60);
    let total = std::sync::atomic::AtomicU64::new(0);
    let got = engine
        .fetch(
            &job,
            &format!("{}/f", r.root),
            &r.out().join("f.part"),
            &rx,
            far,
            &Reservation::new(&total),
        )
        .await
        .unwrap();
    assert!(got.placed);
    assert_eq!(counted.sent.load(Ordering::SeqCst), 1);
}
