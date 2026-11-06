//! The `usnm-boinc gold` command line (#462), and the two offline commands that score an
//! engine on the gold bundle (`gold-stage`, `gold-run`). Their clap definitions and the
//! dispatch to this module's functions live here; `main.rs` only names them.
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum GoldCommands {
    /// Mirror only the 18 locked v1 LoC JPEGs from cache; no BLN600 or work creation.
    MirrorLoc {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
    },
    /// Server side, once per bundle: fetch the original files (one request at a time) to learn
    /// their sizes and md5s and the crops' reference counts; writes prepared.json.
    Prepare {
        /// The bundle tar.
        #[arg(long)]
        bundle: PathBuf,
        /// The campaign root.
        #[arg(long)]
        root: PathBuf,
        #[arg(long, default_value_t = 5)]
        gap_secs: u64,
    },
    /// Make units with stock create_work, one result each.
    Submit {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        /// How many units the campaign has; units already submitted are skipped.
        #[arg(long, default_value_t = 20)]
        units: u32,
        /// Optional targeted gold campaign; requires the server's enable_assignment.
        #[arg(long)]
        target_host: Option<u64>,
        /// The worker image's full digest (sha256:...).
        #[arg(long)]
        engine_image: String,
        /// The Tesseract image the worker image was built on (its TESSERACT_IMAGE).
        #[arg(long)]
        engine_base: String,
        #[arg(long, default_value = "5.5.0")]
        engine_version: String,
        /// Per crop.
        #[arg(long, default_value_t = 300)]
        timeout_secs: u32,
    },
    /// script_validator's init script: check one result against the bundle's expectations.
    Check {
        #[arg(long)]
        file: PathBuf,
        /// The project's gold/ directory, which submit writes.
        #[arg(long)]
        expected_dir: PathBuf,
    },
    /// Write per-host report tables from the canonical results.
    Collect {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Read authoritative result metadata from this private MySQL option file.
        #[arg(long, requires_all = ["mysql_ca_file", "database", "language", "profile"])]
        mysql_defaults_file: Option<PathBuf>,
        #[arg(long, requires_all = ["mysql_defaults_file", "database", "language", "profile"])]
        mysql_ca_file: Option<PathBuf>,
        #[arg(long, requires_all = ["mysql_defaults_file", "mysql_ca_file", "language", "profile"])]
        database: Option<String>,
        /// Explicit trust scope recorded with server-authoritative outcome evidence.
        #[arg(long, requires_all = ["mysql_defaults_file", "mysql_ca_file", "database", "profile"])]
        language: Option<String>,
        #[arg(long, requires_all = ["mysql_defaults_file", "mysql_ca_file", "database", "language"])]
        profile: Option<String>,
    },
    /// Build trust.json from validated canonical reports and native server outcome evidence.
    Trust {
        #[arg(long, required = true)]
        reports: Vec<PathBuf>,
        #[arg(long, required = true)]
        outcomes: Vec<PathBuf>,
        /// The highest overall CER that passes.
        #[arg(long)]
        max_cer: f64,
        /// The exact challenge whose complete reports calibrated this threshold.
        #[arg(long)]
        manifest_sha256: String,
        /// The source language covered by the calibration (three-letter lower-case code).
        #[arg(long)]
        language: String,
        /// The calibrated model profile; Gold uses PSM 3 / 300 DPI.
        #[arg(long)]
        profile: String,
        /// Optional existing trust.json to merge this calibrated scope into.
        #[arg(long)]
        merge: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
}

pub async fn run(command: GoldCommands) -> anyhow::Result<()> {
    use crate::gold as x;
    match command {
        GoldCommands::MirrorLoc { root, project } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&x::mirror::loc(&root, &project).await?)?
            );
        }
        GoldCommands::Prepare {
            bundle,
            root,
            gap_secs,
        } => {
            let p = x::prepare::run(&x::prepare::Options {
                bundle,
                root,
                gap: std::time::Duration::from_secs(gap_secs),
            })
            .await?;
            eprintln!(
                "bundle v{} manifest {}: {} crops, {} original files",
                p.bundle_version,
                p.manifest_sha256,
                p.crops.len(),
                p.remotes.len()
            );
        }
        GoldCommands::Submit {
            root,
            project,
            units,
            target_host,
            engine_image,
            engine_base,
            engine_version,
            timeout_secs,
        } => {
            let s = x::submit::run(&x::submit::Options {
                app: x::submit::APP,
                root,
                project,
                units,
                target_host,
                engine: crate::gold::job::Engine {
                    base: engine_base,
                    dpi: 300,
                    image: engine_image,
                    psm: 3,
                    timeout_secs,
                    version: engine_version,
                },
            })
            .await?;
            eprintln!("{s:?}");
        }
        GoldCommands::Check { file, expected_dir } => x::validate::check(&file, &expected_dir)?,
        GoldCommands::Collect {
            root,
            project,
            spool,
            out,
            mysql_defaults_file,
            mysql_ca_file,
            database,
            language,
            profile,
        } => {
            let options = x::collect::Options {
                root,
                project,
                spool,
                out,
            };
            let summary = match (
                mysql_defaults_file,
                mysql_ca_file,
                database,
                language,
                profile,
            ) {
                (
                    Some(mysql_defaults_file),
                    Some(mysql_ca_file),
                    Some(database),
                    Some(language),
                    Some(profile),
                ) => {
                    let config = x::collect::NativeConfig {
                        mysql_defaults_file,
                        mysql_ca_file,
                        database,
                        language,
                        profile,
                    };
                    let rows = x::collect::native_results(&options.root, &config).await?;
                    x::collect::run_with_native(&options, &rows, &config)?
                }
                (None, None, None, None, None) => x::collect::run(&options)?,
                _ => {
                    anyhow::bail!("native Gold collection requires all database and scope options")
                }
            };
            eprintln!("{summary:?}");
        }
        GoldCommands::Trust {
            reports,
            outcomes,
            max_cer,
            manifest_sha256,
            language,
            profile,
            merge,
            out,
        } => {
            let built = crate::trust::Trust::build_with_outcomes(
                &reports,
                &outcomes,
                max_cer,
                &manifest_sha256,
                crate::trust::Scope {
                    language,
                    profile,
                    psm: 3,
                    dpi: 300,
                },
            )?;
            let t = built.promote_to(&out, merge.as_deref())?;
            for policy in t.effective_policies()? {
                for (image, hosts) in &policy.hosts {
                    eprintln!(
                        "{}/{}: {image}: {} trusted hosts",
                        policy.scope.language,
                        policy.scope.profile,
                        hosts.len()
                    );
                }
            }
        }
    }
    Ok(())
}

// The top-level `gold-stage` command. Its help text is the variant's doc comment in `main.rs`.
#[derive(Args)]
pub struct GoldStageArgs {
    /// The bundle directory, or its tar.
    #[arg(long)]
    bundle: PathBuf,
    #[arg(long)]
    cache: PathBuf,
    #[arg(long)]
    work: PathBuf,
}

pub async fn gold_stage(args: GoldStageArgs) -> anyhow::Result<()> {
    let GoldStageArgs {
        bundle,
        cache,
        work,
    } = args;
    let s = crate::gold::stage(&bundle, &cache, &work).await?;
    println!("{}", serde_json::to_string(&s)?);
    anyhow::ensure!(s.failed.is_empty(), "{} downloads failed", s.failed.len());
    Ok(())
}

// The top-level `gold-run` command. Its help text is the variant's doc comment in `main.rs`.
#[derive(Args)]
pub struct GoldRunArgs {
    #[arg(long)]
    bundle: PathBuf,
    #[arg(long)]
    cache: PathBuf,
    #[arg(long)]
    work: PathBuf,
    /// Where gold-result.json goes.
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value = "tesseract")]
    tesseract: PathBuf,
    /// Executable called directly with IMAGE LANGUAGE arguments; UTF-8 text on stdout.
    #[arg(long, requires_all = ["engine_name", "engine_version"], conflicts_with = "tesseract")]
    engine_cmd: Option<PathBuf>,
    #[arg(long, requires = "engine_cmd")]
    engine_name: Option<String>,
    #[arg(long, requires = "engine_cmd")]
    engine_version: Option<String>,
    #[arg(long, default_value_t = 3)]
    psm: u8,
    #[arg(long, default_value_t = 300)]
    dpi: u32,
    #[arg(long, default_value_t = 600)]
    timeout_secs: u32,
    #[arg(long)]
    limit: Option<usize>,
}

pub async fn gold_run(args: GoldRunArgs) -> anyhow::Result<()> {
    let GoldRunArgs {
        bundle,
        cache,
        work,
        out,
        tesseract,
        engine_cmd,
        engine_name,
        engine_version,
        psm,
        dpi,
        timeout_secs,
        limit,
    } = args;
    let report = crate::gold::run(&crate::gold::RunOptions {
        bundle,
        cache,
        work,
        tesseract,
        command: engine_cmd.map(|executable| crate::gold::CommandEngine {
            executable,
            name: engine_name.expect("required by clap"),
            version: engine_version.expect("required by clap"),
        }),
        psm,
        dpi,
        timeout_secs,
        limit,
    })
    .await?;
    crate::atomic(&out, &serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string(&report.summary)?);
    Ok(())
}
