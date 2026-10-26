//! The conformance runner (13 §13.4.8): `usnm-ocr-worker engines
//! conformance` drives one instance of an engine through the contract's
//! cases, with the worker's own exchange, so every result is checked as a
//! production page's would be (the schema, the paths, the size caps and
//! the layout's limits, 13 §13.4.7 step 1).
//!
//! Not checked yet: nothing left in the container's `cgroup.procs` after
//! a timeout (needs the engine runtime's cgroup, 13 §13.6.6), and
//! contract N-1 (there is only contract 1). Both are reported `skipped`.

use super::*;
use crate::engine::read_failure;
use crate::engine::tail;
use crate::testimg;

/// The line the suite renders and reads back.
pub const LINE: &str = "HELLO LOWER";
/// The fixture the corrupt-image case sends.
pub const CORRUPT: &str = "corrupt.png";
/// How far a box may be from the ink it covers, in pixels: two font
/// pixels of [`testimg`].
const TOLERANCE: f64 = 12.0;
/// The deadline of the deadline case.
const SHORT_DEADLINE: Duration = Duration::from_secs(1);
/// A profile no engine has.
const NO_PROFILE: &str = "usnm-conformance-no-such-profile";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Pass,
    Fail,
    Skipped,
}

/// One case's outcome.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Case {
    pub case: &'static str,
    pub outcome: Outcome,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct Report {
    /// `<engine id> <version>`, from its `hello`.
    pub engine: String,
    pub cases: Vec<Case>,
}

impl Report {
    pub fn passed(&self) -> bool {
        !self.cases.is_empty() && self.cases.iter().all(|c| c.outcome != Outcome::Fail)
    }

    pub fn outcome(&self, case: &str) -> Option<Outcome> {
        self.cases
            .iter()
            .find(|c| c.case == case)
            .map(|c| c.outcome)
    }

    fn push(&mut self, case: &'static str, r: Result<String, String>) {
        let (outcome, detail) = match r {
            Ok(d) => (Outcome::Pass, d),
            Err(d) => (Outcome::Fail, d),
        };
        self.cases.push(Case {
            case,
            outcome,
            detail,
        });
    }

    fn skip(&mut self, case: &'static str, why: &str) {
        self.cases.push(Case {
            case,
            outcome: Outcome::Skipped,
            detail: why.to_owned(),
        });
    }
}

/// A page's answer, and whether its instance was kept.
enum Answer {
    /// The reading, and the text the worker would store from its layout
    /// (13 §13.4.7 step 4), which is what the cases check: the optional
    /// text file isn't stored.
    Read(Box<EngineOutput>, String),
    Kept(EngineError),
    /// The worker killed and removed the instance.
    Dropped(EngineError),
}

impl Answer {
    fn describe(&self) -> String {
        match self {
            Answer::Read(_, text) => format!("read {:?}", text.trim()),
            Answer::Kept(e) => format!("{e:#} (instance kept)"),
            Answer::Dropped(e) => format!("{e:#} (instance replaced)"),
        }
    }
}

fn failure(e: &EngineError) -> (&'static str, &'static str) {
    match e {
        EngineError::Unsupported(_) => ("unsupported", "unsupported"),
        EngineError::Failed(e) => read_failure(e),
    }
}

struct Runner {
    engine: ContractEngine,
    inst: Option<Instance>,
    /// `engine_protocol` failures on any page: a broken line, schema or
    /// limit.
    protocol: Vec<String>,
    pages: u32,
    /// The last page's request to reply on the instance (13 §13.4.8's
    /// deadline): none if it was never sent.
    exchange: Option<Duration>,
}

impl Runner {
    async fn page(
        &mut self,
        bytes: &[u8],
        ext: &str,
        size: (u32, u32),
        language: &str,
        profile: &str,
    ) -> anyhow::Result<Answer> {
        if self.inst.is_none() {
            self.inst = Some(self.engine.instance().await?);
        }
        let inst = self.inst.as_mut().expect("an instance");
        let run_id = crate::provenance::mint_run_id(&self.engine.opts.node);
        self.pages += 1;
        let r = self
            .engine
            .read_bytes(inst, bytes, ext, size, language, profile, &run_id)
            .await;
        self.exchange = inst.last_exchange.take();
        Ok(match r {
            Ok(o) => {
                let text = stored_text(&o, language, self.engine.opts.dpi);
                Answer::Read(Box::new(o), text)
            }
            Err(PageError::Keep(e)) => Answer::Kept(e),
            Err(PageError::Drop(e)) => {
                if failure(&e).1 == "engine_protocol" {
                    self.protocol.push(format!("{e:#}"));
                }
                if let Some(i) = self.inst.take() {
                    i.retire().await;
                }
                Answer::Dropped(e)
            }
        })
    }

    fn stderr(&self) -> String {
        self.inst
            .as_ref()
            .map(|i| i.stderr_tail())
            .unwrap_or_default()
    }
}

/// Runs the suite against the engine `opts` starts, with the fixtures in
/// `fixtures`. One instance, at 300 dpi; `page_timeout` is the deadline of
/// every page but the deadline case's.
pub async fn run(mut opts: ContractOptions, fixtures: &Path) -> anyhow::Result<Report> {
    let corrupt_path = fixtures.join(CORRUPT);
    let corrupt = std::fs::read(&corrupt_path)
        .with_context(|| format!("reading {}", corrupt_path.display()))?;
    opts.instances = 1;
    opts.max_jobs = u32::MAX;
    opts.dpi = 300;
    let mut report = Report::default();
    let engine = match ContractEngine::start(opts).await {
        Ok(e) => e,
        Err(e) => {
            report.push("hello", Err(format!("{e:#}")));
            return Ok(report);
        }
    };
    let h = engine.hello.clone();
    report.engine = format!("{} {}", h.engine.id, h.engine.version);
    report.push(
        "hello",
        Ok(format!(
            "{} {} by {} {}: contract {}, cancel {}, layout {:?}, granularity {}, {} profiles",
            h.engine.id,
            h.engine.version,
            h.adapter.name,
            h.adapter.version,
            h.contract,
            match h.cancel {
                usnm_ocr_contract::engine::Cancel::Adapter => "adapter",
                usnm_ocr_contract::engine::Cancel::Kill => "kill",
            },
            h.layout,
            h.granularity.as_str(),
            h.profiles.len()
        )),
    );
    let mut runner = Runner {
        engine,
        inst: None,
        protocol: Vec::new(),
        pages: 0,
        exchange: None,
    };
    let done = suite(&mut runner, &corrupt, &mut report).await;
    if let Some(i) = runner.inst.take() {
        i.retire().await;
    }
    runner.engine.shutdown().await;
    done?;
    Ok(report)
}

async fn suite(r: &mut Runner, corrupt: &[u8], report: &mut Report) -> anyhow::Result<()> {
    let hello = r.engine.hello.clone();
    let Some((profile, language)) = hello
        .profiles
        .iter()
        .find(|p| p.languages.iter().any(|l| l == "eng"))
        .map(|p| (p.id.clone(), "eng".to_owned()))
        .or_else(|| {
            let p = hello.profiles.first()?;
            Some((p.id.clone(), p.languages.first()?.clone()))
        })
    else {
        report.push("text_line", Err("the hello has no profiles".into()));
        return Ok(());
    };
    let line = testimg::png(&testimg::text_image(LINE));
    let (w, h) = testimg::lines_size(&[LINE]);
    let size = (w as u32, h as u32);

    // A rendered line, its text and its boxes.
    let a = r.page(&line, "png", size, &language, &profile).await?;
    let outcome = match &a {
        Answer::Read(o, text) => check_line(o, text, size),
        other => Err(other.describe()),
    };
    report.push("text_line", outcome);

    // Tesseract runs with the request's dpi; the adapter logs its command
    // line to stderr.
    if hello.engine.family == "tesseract" {
        let mut seen = String::new();
        for _ in 0..20 {
            seen = r.stderr().replace('"', "");
            if seen.contains("--dpi") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        report.push(
            "tesseract_dpi",
            if has_dpi(&seen, "300") {
                Ok("the command line on stderr has --dpi 300".into())
            } else {
                Err(format!("no --dpi 300 on stderr: {}", tail(&seen, 400)))
            },
        );
    } else {
        report.skip("tesseract_dpi", "not a Tesseract engine");
    }

    // A blank page reads as no text.
    let blank = testimg::png(&testimg::blank_image(w, h));
    let outcome = match r.page(&blank, "png", size, &language, &profile).await? {
        Answer::Read(_, text) if text.trim().is_empty() => Ok("no text".into()),
        Answer::Read(_, text) => Err(format!("text on a blank page: {text:?}")),
        other => Err(other.describe()),
    };
    report.push("blank_page", outcome);

    // A corrupt image is `bad_input`.
    let csize = image_size(corrupt).unwrap_or((100, 100));
    let outcome = match r.page(corrupt, "png", csize, &language, &profile).await? {
        Answer::Kept(e) if failure(&e).0 == "bad_input" => Ok(format!("{e:#}")),
        other => Err(format!("not bad_input: {}", other.describe())),
    };
    report.push("corrupt_image", outcome);

    // An unknown profile is `unsupported`.
    let outcome = match r.page(&line, "png", size, &language, NO_PROFILE).await? {
        Answer::Kept(EngineError::Unsupported(_)) => Ok("unsupported".into()),
        other => Err(format!("not unsupported: {}", other.describe())),
    };
    report.push("unknown_profile", outcome);

    // A 1 s deadline on a large page, then the next page.
    let big: Vec<String> = (0..120).map(|_| [LINE; 8].join(" ")).collect();
    let big: Vec<&str> = big.iter().map(String::as_str).collect();
    let (bw, bh) = testimg::lines_size(&big);
    let large = testimg::png(&testimg::lines_image(&big));
    let normal = r.engine.opts.page_timeout;
    r.engine.opts.page_timeout = SHORT_DEADLINE;
    let a = r
        .page(&large, "png", (bw as u32, bh as u32), &language, &profile)
        .await;
    r.engine.opts.page_timeout = normal;
    let a = a?;
    // The adapter's time, request to reply: not the instance's start, the
    // scan's copy, nor reading and converting its output.
    let took = r.exchange.unwrap_or_default();
    let cut = match (hello.cancel, &a) {
        // Read in time: nothing was cut, so there's nothing to check.
        (_, Answer::Read(..)) if took <= SHORT_DEADLINE => Err(None),
        (usnm_ocr_contract::engine::Cancel::Adapter, Answer::Kept(e))
            if failure(e).0 == "timeout" =>
        {
            Ok(format!("timeout from the adapter in {took:.1?}"))
        }
        (usnm_ocr_contract::engine::Cancel::Kill, Answer::Dropped(e))
            if failure(e).1 == "engine_timeout" =>
        {
            Ok(format!(
                "no result, and the worker killed the instance after {took:.1?}"
            ))
        }
        (_, Answer::Read(..)) => Err(Some(format!(
            "a {SHORT_DEADLINE:?} deadline on a {bw}x{bh} page: read after {took:.1?}"
        ))),
        (_, other) => Err(Some(format!(
            "a {SHORT_DEADLINE:?} deadline on a {bw}x{bh} page: {}",
            other.describe()
        ))),
    };
    match cut {
        Ok(d) => {
            let outcome = match r.page(&line, "png", size, &language, &profile).await? {
                Answer::Read(_, text) if text.contains(LINE) => {
                    Ok(format!("{d}; the next page read"))
                }
                other => Err(format!("{d}; the next page: {}", other.describe())),
            };
            report.push("deadline", outcome);
        }
        Err(Some(e)) => report.push("deadline", Err(e)),
        Err(None) => report.skip(
            "deadline",
            &format!(
                "the {bw}x{bh} page read in {took:.1?}, within its {SHORT_DEADLINE:?} deadline: \
                 nothing to cancel"
            ),
        ),
    }
    report.skip(
        "deadline_cgroup",
        "nothing left in the container's cgroup.procs after a timeout isn't checked yet: \
         it needs the engine runtime's cgroup (13 §13.6.6)",
    );

    // Every line on stdout was a contract message, and every result within
    // the size caps; the worker replaces an instance that breaks either.
    let (mut noise, mut limits, mut other) = (Vec::new(), Vec::new(), Vec::new());
    for m in &r.protocol {
        match protocol_rule(m) {
            Rule::Noise => noise.push(m.clone()),
            Rule::Limit => limits.push(m.clone()),
            Rule::Other => other.push(m.clone()),
        }
    }
    let pages = r.pages;
    report.push(
        "protocol",
        if other.is_empty() {
            Ok(format!("{pages} pages: no other protocol failure"))
        } else {
            Err(other.join("; "))
        },
    );
    report.push(
        "stdout_noise",
        if noise.is_empty() {
            Ok(format!("{pages} pages: only contract messages on stdout"))
        } else {
            Err(noise.join("; "))
        },
    );
    report.push(
        "oversized_output",
        if limits.is_empty() {
            Ok(format!(
                "{pages} pages: every result line, layout, text and page directory within the caps"
            ))
        } else {
            Err(limits.join("; "))
        },
    );

    // Closing stdin ends the instance: no more output, and a clean exit.
    if r.inst.is_none() {
        r.inst = Some(r.engine.instance().await?);
    }
    let inst = r.inst.as_mut().expect("an instance");
    inst.close_input().await;
    let outcome =
        match tokio::time::timeout(Duration::from_secs(10), read_line(&mut inst.stdout)).await {
            Err(_) => Err("stdout still open 10 s after stdin closed".to_owned()),
            Ok(Ok(Some(l))) => Err(format!(
                "output after stdin closed: {}",
                tail(&String::from_utf8_lossy(&l), 200)
            )),
            Ok(Err(e)) => Err(format!("{e:#}")),
            Ok(Ok(None)) => match inst.child.as_mut() {
                Some(c) => match tokio::time::timeout(Duration::from_secs(10), c.wait()).await {
                    Ok(Ok(s)) if s.success() => Ok("exited 0".into()),
                    Ok(Ok(s)) => Err(format!("exited with {s}")),
                    Ok(Err(e)) => Err(format!("{e}")),
                    Err(_) => Err("still running 10 s after stdin closed".into()),
                },
                None => Err("no process".into()),
            },
        };
    report.push("stdin_eof", outcome);
    report.skip("contract_n_minus_1", "there is only contract 1");
    Ok(())
}

/// The text `o`'s layout stores as, as the worker derives it.
/// Whether the command line `seen` passes `--dpi` as exactly `dpi`
/// (`--dpi 300` or `--dpi=300`, however the adapter quotes it).
fn has_dpi(seen: &str, dpi: &str) -> bool {
    let tokens: Vec<&str> = seen
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '[' | ']' | '\'' | '"'))
        .filter(|t| !t.is_empty())
        .collect();
    tokens.iter().enumerate().any(|(i, t)| {
        t.strip_prefix("--dpi=") == Some(dpi) || (*t == "--dpi" && tokens.get(i + 1) == Some(&dpi))
    })
}

fn stored_text(o: &EngineOutput, language: &str, dpi: u32) -> String {
    let Some(page) = o.page.clone() else {
        return String::new();
    };
    crate::stored::store_from(
        page,
        o.layout_from,
        &o.engine,
        Some(&o.read),
        language,
        dpi,
        &crate::stored::Source {
            url: "conformance",
            sha256: "",
            md5: None,
            run_id: None,
            damage_before: None,
        },
        &crate::stored::worker_version(),
        chrono::Utc::now(),
    )
    .text
}

enum Rule {
    /// Something on stdout that isn't a contract message.
    Noise,
    /// A result line, file, page directory or layout over its cap or limit.
    Limit,
    Other,
}

/// Which of the contract's rules an `engine_protocol` failure broke, from
/// the messages of `usnm_ocr_contract` and `usnm_alto` and the worker's
/// file checks.
fn protocol_rule(m: &str) -> Rule {
    const NOISE: [&str; 2] = ["a line that isn't contract JSON", "a second hello"];
    const LIMIT: [&str; 7] = [
        "a line over",
        "is over",
        "holds over",
        "breaks a limit",
        "over the size limit",
        "nested too deeply",
        // `too many elements`, `an element has too many attributes`.
        "too many",
    ];
    if NOISE.iter().any(|n| m.contains(n)) {
        Rule::Noise
    } else if LIMIT.iter().any(|n| m.contains(n)) {
        Rule::Limit
    } else {
        Rule::Other
    }
}

/// The line's text, and boxes at the page's granularity on its ink.
fn check_line(o: &EngineOutput, text: &str, (w, h): (u32, u32)) -> Result<String, String> {
    if !text.contains(LINE) {
        return Err(format!("read {text:?}, not {LINE:?}"));
    }
    let page = o.page.as_ref().ok_or("no layout")?;
    let (w, h) = (f64::from(w), f64::from(h));
    let inside = |r: &usnm_alto::Rect| {
        r.x0 >= -TOLERANCE && r.y0 >= -TOLERANCE && r.x1 <= w + TOLERANCE && r.y1 <= h + TOLERANCE
    };
    let near = |r: &usnm_alto::Rect, e: &[f64; 4]| {
        (r.x0 - e[0]).abs() <= TOLERANCE
            && (r.y0 - e[1]).abs() <= TOLERANCE
            && (r.x1 - e[2]).abs() <= TOLERANCE
            && (r.y1 - e[3]).abs() <= TOLERANCE
    };
    let covers = |r: &usnm_alto::Rect, e: &[f64; 4]| {
        r.x0 <= e[0] + TOLERANCE
            && r.y0 <= e[1] + TOLERANCE
            && r.x1 >= e[2] - TOLERANCE
            && r.y1 >= e[3] - TOLERANCE
    };
    for b in &page.blocks {
        let all = std::iter::once(&b.rect).chain(
            b.lines
                .iter()
                .flat_map(|l| std::iter::once(&l.rect).chain(l.words.iter().map(|w| &w.rect))),
        );
        if let Some(r) = all.into_iter().find(|r| !inside(r)) {
            return Err(format!("a box outside the {w}x{h} image: {r:?}"));
        }
    }
    let words = testimg::word_boxes(LINE);
    let first = words.first().map(|(_, b)| *b).ok_or("no words")?;
    let last = words.last().map(|(_, b)| *b).ok_or("no words")?;
    let line = [first[0], first[1], last[2], last[3]];
    let g = page.granularity;
    match g {
        usnm_alto::Granularity::Word => {
            for (word, e) in &words {
                let found = page
                    .blocks
                    .iter()
                    .flat_map(|b| &b.lines)
                    .flat_map(|l| &l.words)
                    .find(|w| w.text.trim() == word)
                    .ok_or_else(|| format!("no word {word:?}"))?;
                if !near(&found.rect, e) {
                    return Err(format!("{word:?} at {:?}, not near {e:?}", found.rect));
                }
            }
        }
        usnm_alto::Granularity::Line => {
            let found = page
                .blocks
                .iter()
                .flat_map(|b| &b.lines)
                .find(|l| l.words.iter().any(|w| w.text.contains(&words[0].0)))
                .ok_or("no line with the text")?;
            if !near(&found.rect, &line) {
                return Err(format!("the line at {:?}, not near {line:?}", found.rect));
            }
        }
        usnm_alto::Granularity::Block | usnm_alto::Granularity::Page => {
            if !page
                .blocks
                .iter()
                .filter(|b| b.kind.is_text())
                .any(|b| covers(&b.rect, &line))
            {
                return Err(format!("no text block covers the line at {line:?}"));
            }
        }
    }
    Ok(format!(
        "read {LINE:?}, {} boxes within {TOLERANCE} px",
        g.as_str()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The deadline case times the adapter, request to reply: not the
    /// instance's start before it, nor the worker's work on the scan.
    #[tokio::test]
    async fn the_exchange_is_timed_without_the_worker_side() {
        let dir = tempfile::tempdir().unwrap();
        let engine = super::super::tests::script_engine_with(
            dir.path(),
            "sleep 1.2",
            r#"while IFS= read -r l; do
  run=$(printf '%s\n' "$l" | sed 's/.*"run_id":"\([^"]*\)".*/\1/')
  sleep 0.2
  printf '{"engine_stats":{},"error":{"kind":"timeout","message":"timeout"},"markup":"none","run_id":"%s","status":"timeout","type":"result","warnings":[]}\n' "$run"
done
"#,
        )
        .await;
        // The page starts its own instance, as after one was replaced.
        let idle = engine.idle.lock().unwrap().pop().unwrap();
        idle.retire().await;
        let mut r = Runner {
            engine,
            inst: None,
            protocol: Vec::new(),
            pages: 0,
            exchange: None,
        };
        let big: Vec<String> = (0..120).map(|_| [LINE; 8].join(" ")).collect();
        let big: Vec<&str> = big.iter().map(String::as_str).collect();
        let (bw, bh) = testimg::lines_size(&big);
        let large = testimg::png(&testimg::lines_image(&big));
        let t = std::time::Instant::now();
        let a = r
            .page(&large, "png", (bw as u32, bh as u32), "eng", "eng")
            .await
            .unwrap();
        let wall = t.elapsed();
        assert!(matches!(a, Answer::Kept(_)), "{}", a.describe());
        let took = r.exchange.expect("timed");
        assert!(wall >= Duration::from_millis(1400), "{wall:?}");
        assert!(
            took >= Duration::from_millis(200) && took < Duration::from_secs(1),
            "{took:?} of {wall:?}"
        );
        // A page that never reaches the adapter has no exchange.
        let a = r.page(b"", "png", (0, 0), "eng", "eng").await.unwrap();
        assert!(!matches!(a, Answer::Read(..)));
        assert_eq!(r.exchange, None);
        if let Some(i) = r.inst.take() {
            i.retire().await;
        }
        r.engine.shutdown().await;
    }

    #[test]
    fn the_dpi_must_be_its_own_argument() {
        for ok in [
            "tesseract page.png out --dpi 300 -l eng",
            "\"tesseract\" \"--dpi\" \"300\"",
            "[\"tesseract\", \"--dpi\", \"300\"]",
            "tesseract --dpi=300",
        ] {
            assert!(has_dpi(ok, "300"), "{ok}");
        }
        for bad in [
            "tesseract --dpi 3000",
            "tesseract --dpi 30",
            "tesseract --dpi=3000",
            "tesseract --dpi",
            "tesseract --dpix 300",
            "tesseract 300 --dpi 72",
        ] {
            assert!(!has_dpi(bad, "300"), "{bad}");
        }
    }

    #[test]
    fn protocol_failures_are_sorted_by_rule() {
        let rule = |m: &str| match protocol_rule(m) {
            Rule::Noise => "noise",
            Rule::Limit => "limit",
            Rule::Other => "other",
        };
        let noise = usnm_ocr_contract::engine::decode(b"loading model\n").unwrap_err();
        assert_eq!(rule(&format!("engine protocol: {noise}")), "noise");
        assert_eq!(rule("engine protocol: a second hello"), "noise");
        let long = usnm_ocr_contract::engine::ProtocolError::TooLong;
        assert_eq!(rule(&format!("engine protocol: {long}")), "limit");
        let boxes = usnm_alto::boxes::BoxesError::Limit("20001 blocks".into());
        assert_eq!(rule(&format!("engine protocol: {boxes}")), "limit");
        for e in [
            usnm_alto::AltoError::TooLarge,
            usnm_alto::AltoError::TooDeep,
            usnm_alto::AltoError::TooManyElements,
            usnm_alto::AltoError::TooManyAttributes,
        ] {
            let e = usnm_alto::ParseError::Alto(e);
            assert_eq!(rule(&format!("engine protocol: {e}")), "limit", "{e}");
        }
        let e = usnm_alto::ParseError::Limit("strings per page");
        assert_eq!(rule(&format!("engine protocol: {e}")), "limit");
        assert_eq!(rule("page.txt is over 8388608 bytes"), "limit");
        assert_eq!(
            rule("engine protocol: the page's directory holds over 1 bytes"),
            "limit"
        );
        let wrong = usnm_ocr_contract::engine::ProtocolError::NoLayout;
        assert_eq!(rule(&format!("engine protocol: {wrong}")), "other");
    }
}
