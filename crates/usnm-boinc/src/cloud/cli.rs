//! The `usnm-boinc cloud` command line. `main.rs` only names the group.
use super::pull::{self, Config};
use super::pull_results;
use super::spool_out;
use super::submit as cloud_submit;
#[cfg(any(test, feature = "cloud-validatord"))]
use super::validatord;
use anyhow::{bail, Context};
use clap::{Args, Subcommand};
use std::{path::PathBuf, time::Duration};

#[derive(Subcommand)]
pub enum CloudCommands {
    /// Pull finished workunits from the cloud server into a stock-layout spool, then
    /// acknowledge them (unless --no-ack). Dev cloud server only.
    Pull(PullArgs),
    /// Pull finished workunits the cloud server exported to a results bucket into the same
    /// spool, then delete them from the bucket (unless --no-delete). Dev results bucket only.
    PullResults(PullResultsArgs),
    /// Hand the spool `cloud pull` wrote to the hand-off store with `handoff::put_spool`, one
    /// finished single-page job at a time. Dev cloud server only.
    SpoolOut(SpoolOutArgs),
    /// Send usnm_ocr jobs the controller already exported (`jobs/<id>/` under --root), or with
    /// --app usnm_ndlocr the staged units (`units/<doc>/`), to the cloud server's admin API.
    /// Reads the on-prem state, never changes it. Dev cloud server only.
    Submit(SubmitArgs),
    /// The cloud server's validation webhook for the gold apps (POST /v1/validate/<app>). Dev
    /// only; nothing in the running bridge calls it.
    #[cfg(any(test, feature = "cloud-validatord"))]
    Validatord(ValidatordArgs),
}

#[cfg(any(test, feature = "cloud-validatord"))]
#[derive(Args)]
pub struct ValidatordArgs {
    /// Where to listen. Loopback by default: the service speaks plain http, so put TLS in front.
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: std::net::SocketAddr,
    /// Allow a non-loopback --listen (the bearer token would then cross that network in clear).
    #[arg(long)]
    allow_non_loopback: bool,
    /// File holding the webhook bearer token the cloud server sends (its WEBHOOK_TOKEN).
    #[arg(long)]
    secret_file: PathBuf,
    /// File holding the webhook signing secret (the server's WEBHOOK_SIGNING_SECRET). When set,
    /// a call needs a valid X-CB-Signature and a timestamp within 5 minutes; when not, calls are
    /// checked by the bearer token only.
    #[arg(long)]
    signing_secret_file: Option<PathBuf>,
    /// The durable state: verdicts by call id and quality's seen copies and pair lines. A local
    /// directory, or `r2://<bucket>[/<prefix>]` with R2_ENDPOINT, R2_ACCESS_KEY_ID and
    /// R2_SECRET_ACCESS_KEY set (`usnm_store::open`).
    #[arg(long)]
    state_url: String,
    /// Local scratch space for each call's fetched outputs.
    #[arg(long, default_value = "/tmp/usnm-validatord")]
    work_dir: PathBuf,
    /// The project's gold/ directory, which `gold submit` writes.
    #[arg(long)]
    gold_dir: PathBuf,
    /// The cloud server's root (https unless loopback), to fetch outputs from.
    #[arg(long)]
    base_url: String,
    /// File holding the admin bearer token for output reads, or CBOINC_TOKEN.
    #[arg(long)]
    token_file: Option<PathBuf>,
    #[arg(long, default_value_t = 1 << 20)]
    max_body_bytes: usize,
    /// Per call; the server's `<webhook_timeout_secs>` is 20 by default.
    #[arg(long, default_value_t = 15)]
    timeout_secs: u64,
}

#[derive(Args)]
pub struct SpoolOutArgs {
    /// The spool root `cloud pull` writes (`results/<batch>/` under it).
    #[arg(long)]
    spool_dir: PathBuf,
    /// The hand-off store, as `handoff put-spool --to-url`. Not needed with --dry-run.
    #[arg(long)]
    to_url: Option<String>,
    /// A gold/trust.json to put with each job, as the bridge does when the share has one.
    #[arg(long)]
    trust: Option<PathBuf>,
    /// List what would be handed off; touch nothing.
    #[arg(long)]
    dry_run: bool,
    /// Hand off again even where the ledger says it was done.
    #[arg(long)]
    redo: bool,
    /// Make one pass and exit. Without it, pass again every --interval-secs.
    #[arg(long)]
    once: bool,
    #[arg(long, default_value_t = 60)]
    interval_secs: u64,
}

#[derive(Args)]
pub struct PullArgs {
    /// Server root, e.g. https://boinc-dev.example.org (the /admin/v1 path is added).
    #[arg(long)]
    base_url: String,
    /// File holding the admin bearer token. Without it the CBOINC_TOKEN environment variable
    /// is used. The token is never taken from the command line and never logged.
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// Spool root: files go to `results/<batch>/`, staging to `.incoming/`.
    #[arg(long)]
    spool_dir: PathBuf,
    /// Shadow mode: write the spool but never acknowledge.
    #[arg(long)]
    no_ack: bool,
    /// Stop after this many workunits in total (per pass).
    #[arg(long)]
    limit: Option<usize>,
    /// Application to pull; repeat for several. Without it, every app is pulled.
    #[arg(long = "app")]
    apps: Vec<String>,
    /// Workunits per request (1 to 500).
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
    page_size: u32,
    /// Make one pass and exit. Without it, pass again every --interval-secs.
    #[arg(long)]
    once: bool,
    #[arg(long, default_value_t = 60)]
    interval_secs: u64,
}

#[derive(Args)]
pub struct PullResultsArgs {
    /// The results bucket: `r2://<bucket>[/<prefix>]` (R2_ENDPOINT, R2_ACCESS_KEY_ID and
    /// R2_SECRET_ACCESS_KEY set, as `usnm_store::open` reads them) or a local directory.
    #[arg(long)]
    results_url: String,
    /// Spool root: files go to `results/<batch>/`, staging to `.incoming/`.
    #[arg(long)]
    spool_dir: PathBuf,
    /// Shadow mode: write the spool but leave the ledgers and files in the bucket.
    #[arg(long)]
    no_delete: bool,
    /// Stop after this many ledgers (per pass).
    #[arg(long)]
    limit: Option<usize>,
    /// Application to pull; repeat for several. Without it, every app is pulled.
    #[arg(long = "app")]
    apps: Vec<String>,
    /// Make one pass and exit. Without it, pass again every --interval-secs.
    #[arg(long)]
    once: bool,
    #[arg(long, default_value_t = 60)]
    interval_secs: u64,
}

#[derive(Args)]
pub struct SubmitArgs {
    /// The controller root, with jobs at `<root>/jobs/<id>/` (usnm_ocr); for usnm_ndlocr, the
    /// ndlocr root, with units at `<root>/units/<doc>/`.
    #[arg(long)]
    root: PathBuf,
    /// The app to submit.
    #[arg(long, value_enum, default_value_t)]
    app: cloud_submit::App,
    /// Server root, e.g. https://boinc-dev.example.org (the /admin/v1 path is added).
    #[arg(long)]
    base_url: String,
    /// File holding the admin bearer token, or the CBOINC_TOKEN environment variable. Never
    /// taken from the command line and never logged. Not needed with --dry-run.
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// A job id (sha256) to send, or for usnm_ndlocr a doc id; repeat for several.
    #[arg(long = "id", conflicts_with = "all", required_unless_present = "all")]
    ids: Vec<String>,
    /// Send every record (usnm_ocr) or unit (usnm_ndlocr) under --root.
    #[arg(long)]
    all: bool,
    /// Print what would be sent; make no request.
    #[arg(long)]
    dry_run: bool,
    /// Jobs per create request (1 to 500).
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
    batch_size: u32,
}

fn read_token(file: &Option<PathBuf>, env: &str) -> anyhow::Result<String> {
    let t = match file {
        Some(p) => std::fs::read_to_string(p)
            .with_context(|| format!("reading token file {}", p.display()))?,
        None => std::env::var(env).with_context(|| format!("give a token file or set {env}"))?,
    };
    let t = t.trim().to_owned();
    if t.is_empty() {
        bail!("the token is empty");
    }
    Ok(t)
}

/// The webhook signing secret, as the server keys its HMAC: the file's bytes unchanged (unlike a
/// token, no whitespace is trimmed), except one final line ending, which an editor adds and
/// the container's `printf '%s'` never writes. An empty secret is refused.
#[cfg(any(test, feature = "cloud-validatord"))]
fn read_signing_secret(path: &std::path::Path) -> anyhow::Result<String> {
    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut s = String::from_utf8(raw).context("the signing secret is not UTF-8")?;
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    if s.is_empty() {
        bail!("the signing secret is empty");
    }
    Ok(s)
}

pub async fn run(command: CloudCommands) -> anyhow::Result<()> {
    match command {
        #[cfg(any(test, feature = "cloud-validatord"))]
        CloudCommands::Validatord(a) => {
            if !a.listen.ip().is_loopback() && !a.allow_non_loopback {
                bail!("--listen is not a loopback address; add --allow-non-loopback to serve it");
            }
            let secret = read_token(&Some(a.secret_file), "")?;
            let signing_secret = match a.signing_secret_file {
                Some(p) => Some(read_signing_secret(&p)?),
                None => {
                    eprintln!(
                        "warning: no --signing-secret-file: webhook signatures are not verified, \
                         so a call is authenticated by the bearer token alone and a captured call \
                         can be replayed"
                    );
                    None
                }
            };
            let admin = read_token(&a.token_file, "CBOINC_TOKEN")?;
            let fetch =
                pull::Config::new(&a.base_url, admin, a.work_dir.clone(), false, vec![], 1)?;
            let state = usnm_store::open(&a.state_url)
                .with_context(|| format!("opening the state store {}", a.state_url))?;
            let cfg = validatord::Config {
                secret,
                signing_secret,
                state,
                work_dir: a.work_dir,
                gold_dir: a.gold_dir,
                fetch,
                max_body: a.max_body_bytes,
                call_timeout: Duration::from_secs(a.timeout_secs),
            };
            let listener = tokio::net::TcpListener::bind(a.listen).await?;
            eprintln!("validatord listening on {}", listener.local_addr()?);
            validatord::serve(cfg, listener).await
        }
        CloudCommands::SpoolOut(a) => {
            let cfg = spool_out::Config {
                spool: a.spool_dir,
                to_url: a.to_url,
                trust: a.trust,
                dry_run: a.dry_run,
                redo: a.redo,
            };
            loop {
                let report = spool_out::spool_out_once(&cfg).await?;
                println!("{report}");
                if report.ignored > 0 {
                    eprintln!(
                        "{} name(s) under results/ are not a job's spool file, for example: {}",
                        report.ignored,
                        report.ignored_examples.join(", ")
                    );
                }
                if !report.failed.is_empty() {
                    bail!("{} job(s) not handed off", report.failed.len());
                }
                if a.once || a.dry_run {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_secs(a.interval_secs)).await;
            }
        }
        CloudCommands::Submit(a) => {
            let token = if a.dry_run {
                String::new()
            } else {
                read_token(&a.token_file, "CBOINC_TOKEN")?
            };
            let cfg = cloud_submit::Config::new(&a.base_url, token, a.batch_size, a.dry_run)?
                .with_app(a.app);
            let report = cloud_submit::run(&cfg, &a.root, &a.ids, a.all).await?;
            println!("{report}");
            for line in &report.lines {
                eprintln!("{line}");
            }
            if report.failed > 0 {
                bail!("{} job(s) not submitted", report.failed);
            }
            Ok(())
        }
        CloudCommands::PullResults(a) => {
            let store = usnm_store::open(&a.results_url)
                .with_context(|| format!("opening the results bucket {}", a.results_url))?;
            let cfg = pull_results::Config {
                store,
                spool: a.spool_dir,
                apps: a.apps,
                no_delete: a.no_delete,
            };
            loop {
                let report = pull_results::pull_results_once(&cfg, a.limit).await?;
                println!("{report}");
                if !report.failed.is_empty() {
                    bail!("{} ledger(s) not pulled", report.failed.len());
                }
                if a.once {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_secs(a.interval_secs)).await;
            }
        }
        CloudCommands::Pull(a) => {
            let token = read_token(&a.token_file, "CBOINC_TOKEN")?;
            let cfg = Config::new(
                &a.base_url,
                token,
                a.spool_dir,
                !a.no_ack,
                a.apps,
                a.page_size,
            )?;
            loop {
                let stats = pull::pull_once(&cfg, a.limit).await?;
                println!("{stats}");
                if !stats.failed.is_empty() {
                    bail!("{} workunit(s) not pulled", stats.failed.len());
                }
                if a.once {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_secs(a.interval_secs)).await;
            }
        }
    }
}

#[cfg(test)]
mod signing_secret_tests {
    use super::read_signing_secret;

    fn read(bytes: &[u8]) -> anyhow::Result<String> {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s");
        std::fs::write(&p, bytes).unwrap();
        read_signing_secret(&p)
    }

    #[test]
    fn the_signing_secret_keeps_its_whitespace_and_drops_one_line_ending() {
        assert_eq!(read(b" sek ").unwrap(), " sek ");
        assert_eq!(read(b" sek \n").unwrap(), " sek ");
        assert_eq!(read(b"sek\r\n").unwrap(), "sek");
        assert_eq!(read(b"sek\n\n").unwrap(), "sek\n");
        assert!(read(b"").is_err());
        assert!(read(b"\n").is_err());
    }
}
