use anyhow::Context;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use usnm_ocr_survey::{bench, bundle, score};

#[derive(Parser)]
#[command(
    about = "OCR gold set: CER/WER scoring (as gold.py score) and the volunteer gold bundle (#457)"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// CER and WER of hypotheses against crops.jsonl's transcriptions; replaces `gold.py score`.
    Score {
        #[arg(long)]
        crops: PathBuf,
        /// ENGINE=FILE.jsonl, rows {"crop_id", "text"}; repeatable.
        #[arg(long)]
        hyp: Vec<String>,
        /// Also score the engines' texts stored in crops.jsonl's hints.
        #[arg(long)]
        hints: bool,
        /// Skip the second-pass reviewer check.
        #[arg(long)]
        allow_unreviewed: bool,
        /// Directory for cer-by-crop.csv and cer-summary.csv.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Builds the volunteer gold bundle (directory, reproducible tar and its sha256) from
    /// `gold.py public-convert` outputs.
    BundleBuild {
        /// NAME=DIR: a public-convert output directory for one source; repeatable.
        #[arg(long, required = true)]
        source: Vec<String>,
        /// NAME=FILE: the original archive of a source that ships references (BLN600's zip),
        /// checked against SOURCE.json and read for the member checksums.
        #[arg(long)]
        original: Vec<String>,
        #[arg(long)]
        version: u32,
        #[arg(long, default_value_t = 457)]
        seed: u64,
        /// The most crops taken from each source.
        #[arg(long, default_value_t = 30)]
        per_source: usize,
        #[arg(long)]
        out: PathBuf,
        /// Also write the lock (checksums, ids, no dataset text) for the repository here.
        #[arg(long)]
        lock: Option<PathBuf>,
    },
    /// Verifies a bundle directory or tar: the manifest and every file's sha256.
    BundleVerify {
        /// The bundle directory, or its tar.
        #[arg(long)]
        bundle: PathBuf,
        /// Where a tar is unpacked (default: a temporary directory).
        #[arg(long)]
        work: Option<PathBuf>,
    },
    /// Builds an OCR benchmark release (#738) from its spec, input inventory and input root, then
    /// verifies it. Prints the release's entry for ocr/bench/releases.json.
    BenchBuild {
        /// ocr/bench/<release>/spec.json
        #[arg(long)]
        spec: PathBuf,
        /// ocr/bench/<release>/inputs.sha256: every input file and its sha256.
        #[arg(long)]
        inventory: PathBuf,
        /// The directory the inventory's paths are relative to.
        #[arg(long)]
        inputs: PathBuf,
        /// Where the release directory and its tar are written.
        #[arg(long)]
        out: PathBuf,
    },
    /// Verifies a benchmark release directory or tar: SHA256SUMS, the manifest's rules and the
    /// example scores; with --index, also its entry in ocr/bench/releases.json.
    BenchVerify {
        #[arg(long)]
        release: PathBuf,
        #[arg(long)]
        index: Option<PathBuf>,
        /// Where a tar is unpacked (default: a temporary directory).
        #[arg(long)]
        work: Option<PathBuf>,
    },
    /// Lists a subset's samples as JSONL (id, image path and sha256) for an engine runner.
    BenchSelect {
        /// A verified release directory.
        #[arg(long)]
        release: PathBuf,
        #[arg(long)]
        subset: String,
        /// Also list each reference's path. Keep references out of model prompts.
        #[arg(long)]
        with_references: bool,
    },
    /// Scores engine outputs (JSONL rows {"id", "text"}) on a subset; missing outputs are counted.
    BenchScore {
        /// A verified release directory.
        #[arg(long)]
        release: PathBuf,
        #[arg(long)]
        subset: String,
        #[arg(long)]
        outputs: PathBuf,
        /// legacy-ws-v1 (the Iris pilot's whitespace-collapsed CER) or raw-v1.
        #[arg(long, default_value = "legacy-ws-v1")]
        rule: String,
        /// Write the per-sample report here; stdout gets the totals.
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Downloads the original files a bundle refers to into a cache, checking each sha256.
    BundleStage {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        cache: PathBuf,
        /// Seconds between downloads.
        #[arg(long, default_value_t = 1.0)]
        gap_secs: f64,
    },
}

fn pairs(specs: &[String], what: &str) -> anyhow::Result<Vec<(String, PathBuf)>> {
    specs
        .iter()
        .map(|s| {
            s.split_once('=')
                .filter(|(n, p)| !n.is_empty() && !p.is_empty())
                .map(|(n, p)| (n.to_string(), PathBuf::from(p)))
                .with_context(|| format!("--{what} wants NAME=PATH, got {s:?}"))
        })
        .collect()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Commands::Score {
            crops,
            hyp,
            hints,
            allow_unreviewed,
            out,
        } => {
            let rows = score::read_jsonl(&crops)?;
            let hyps = score::load_hyps(&rows, &hyp, hints)?;
            let s = score::score(&rows, &hyps, !allow_unreviewed)?;
            eprintln!(
                "{} of {} crops usable; {} left out (failed validation)",
                s.used,
                s.rows,
                s.rows - s.used
            );
            if let Some(out) = out {
                score::write_csvs(&out, &s).context("writing the CSV files")?;
            }
            print!("{}", score::table(&s.summary));
        }
        Commands::BundleBuild {
            source,
            original,
            version,
            seed,
            per_source,
            out,
            lock,
        } => {
            let originals: std::collections::BTreeMap<_, _> =
                pairs(&original, "original")?.into_iter().collect();
            let inputs = pairs(&source, "source")?
                .into_iter()
                .map(|(name, dir)| bundle::Input {
                    original: originals.get(&name).cloned(),
                    source: name,
                    dir,
                })
                .collect();
            let b = bundle::build(&bundle::BuildOptions {
                inputs,
                version,
                seed,
                per_source,
                out,
            })?;
            let lock_json = bundle::lock(&b)?;
            if let Some(p) = lock {
                std::fs::write(&p, serde_json::to_string_pretty(&lock_json)? + "\n")?;
            }
            println!(
                "{}",
                serde_json::json!({
                    "crops": b.manifest.crops.len(),
                    "crops_per_source": lock_json["crops_per_source"],
                    "dir": b.dir,
                    "manifest_sha256": lock_json["manifest_sha256"],
                    "remotes": b.manifest.remotes.len(),
                    "tar": b.tar,
                    "tar_bytes": lock_json["tar_bytes"],
                    "tar_sha256": b.tar_sha256,
                })
            );
        }
        Commands::BundleVerify { bundle: path, work } => {
            let tmp = tempfile::tempdir()?;
            let dir = if path.is_file() {
                bundle::unpack(&path, work.as_deref().unwrap_or(tmp.path()))?
            } else {
                path
            };
            let m = bundle::verify(&dir)?;
            println!(
                "{}",
                serde_json::json!({
                    "bundle_version": m.bundle_version,
                    "crops": m.crops.len(),
                    "files": m.files.len(),
                    "manifest_sha256": m.digest()?,
                    "remotes": m.remotes.len(),
                    "verified": true,
                })
            );
        }
        Commands::BenchBuild {
            spec,
            inventory,
            inputs,
            out,
        } => {
            let b = bench::build(&bench::BuildOptions {
                spec,
                inventory,
                inputs,
                out,
            })?;
            eprintln!("built {} and {}", b.dir.display(), b.archive.display());
            println!("{}", serde_json::to_string_pretty(&b.index)?);
        }
        Commands::BenchVerify {
            release,
            index,
            work,
        } => {
            let tmp = tempfile::tempdir()?;
            let dir = if release.is_file() {
                bench::unpack(&release, work.as_deref().unwrap_or(tmp.path()))?
            } else {
                release
            };
            let v = bench::verify(&dir)?;
            if let Some(index) = index {
                let idx: serde_json::Value = serde_json::from_slice(&std::fs::read(&index)?)
                    .context("parsing the release index")?;
                bench::check_index(&idx, &v, &bench::load(&dir)?)?;
            }
            println!("{}", serde_json::to_string(&v)?);
        }
        Commands::BenchSelect {
            release,
            subset,
            with_references,
        } => {
            let m = bench::load(&release)?;
            for row in bench::select(&release, &m, &subset, with_references)? {
                println!("{}", serde_json::to_string(&row)?);
            }
        }
        Commands::BenchScore {
            release,
            subset,
            outputs,
            rule,
            report,
        } => {
            let m = bench::load(&release)?;
            let outputs = bench::read_outputs(&std::fs::read_to_string(&outputs)?)?;
            let (_, r) = bench::score(&release, &m, &subset, &outputs, &rule)?;
            if let Some(p) = report {
                std::fs::write(&p, serde_json::to_string_pretty(&r)? + "\n")?;
            }
            let mut totals = r;
            totals.as_object_mut().map(|o| o.remove("rows"));
            println!("{}", serde_json::to_string_pretty(&totals)?);
        }
        Commands::BundleStage {
            bundle: dir,
            cache,
            gap_secs,
        } => {
            let m = bundle::verify(&dir)?;
            let s = bundle::stage(
                &m,
                &cache,
                std::time::Duration::try_from_secs_f64(gap_secs)
                    .context("--gap-secs must be a non-negative number of seconds")?,
            )
            .await?;
            println!("{}", serde_json::to_string(&s)?);
            anyhow::ensure!(s.failed.is_empty(), "{} downloads failed", s.failed.len());
        }
    }
    Ok(())
}
