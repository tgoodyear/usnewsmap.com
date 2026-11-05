//! The `usnm-boinc qwen` command line (#522): its clap definitions and the dispatch to
//! this module's stages. `main.rs` only names the group.
use anyhow::Context;
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum QwenCommands {
    /// Audit explicitly selected retained readings, never selecting a canonical result.
    Candidates {
        #[arg(long)]
        job: PathBuf,
        #[arg(long)]
        page: PathBuf,
        /// Directories written by `qwen check --keep`, at most 16.
        #[arg(long, required = true, num_args = 1..)]
        candidate: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Stage public gold crops without sending reference text to volunteers.
    BenchmarkStage {
        #[arg(long)]
        crops: PathBuf,
        #[arg(long)]
        root: PathBuf,
        /// JSON Engine with immutable image/llama/weights digests and exactly one model.
        #[arg(long)]
        engine: PathBuf,
        #[arg(long, default_value_t = 20)]
        max_crops: usize,
        #[arg(long, default_value_t = 1800)]
        timeout_secs: u32,
        /// `sample` names each crop by its dataset sample id; a campaign keeps the spelling
        /// it was staged with.
        #[arg(long, value_enum, default_value_t = crate::qwen::benchmark::DocIds::Legacy)]
        doc_ids: crate::qwen::benchmark::DocIds,
    },
    /// Score canonical benchmark outputs; missing hypotheses count as deletions.
    BenchmarkCollect {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Cut each staged quality page's crops (the same seed) and write its unit.
    Stage {
        /// The usnm_quality staging root whose units to cut crops from.
        #[arg(long)]
        quality_root: PathBuf,
        /// Ordered JSON array of source doc_ids; never scan unrelated quality pages.
        #[arg(long)]
        members: Option<PathBuf>,
        #[arg(long)]
        root: PathBuf,
        /// The worker image's registry digest (sha256:...).
        #[arg(long)]
        engine_image: String,
        /// The llama.cpp server image it is built on (its USNM_QWEN_LLAMA_IMAGE).
        #[arg(long)]
        engine_llama: String,
        /// Its USNM_QWEN_WEIGHTS.
        #[arg(long)]
        engine_weights: String,
        /// GPU runtime per page, including all crops; allow slower volunteer GPUs.
        #[arg(long, default_value_t = 14_400)]
        gpu_timeout_secs: u32,
        #[arg(long, default_value_t = 7200)]
        cpu_timeout_secs: u32,
        /// Output tokens allowed per 1,000 square pixels of crop.
        #[arg(long, default_value_t = 4)]
        max_tokens_per_1000px2: u32,
        /// The request version to stage. Every dispatchable app version must read it.
        #[arg(long, default_value_t = usnm_ocr_contract::qwen::PROMPT_VERSION)]
        prompt_version: u32,
        #[arg(long, default_value_t = 20)]
        max_pages: usize,
        /// Pack staged pages into multi-page bundle workunits of up to this many pages (1 to 32),
        /// each bundle's qwen.json within the 512 KiB the workers read.
        #[arg(long, default_value_t = 1)]
        bundle_size: usize,
    },
    /// Hand staged units to create_work, two results each.
    Submit {
        /// Ordered JSON array of source doc_ids; never publish unrelated pending jobs.
        #[arg(long)]
        members: Option<PathBuf>,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        #[arg(long, default_value_t = 20)]
        max_units: usize,
        #[arg(long, value_enum, default_value_t = crate::qwen::submit::Target::Container)]
        target: crate::qwen::submit::Target,
        #[arg(long)]
        model_dir: Option<PathBuf>,
    },
    /// Write a unit from a local page and ALTO, to check a host or the image without LoC or BOINC.
    /// Inside the worker image the engine values default to the image's own.
    LocalUnit {
        #[arg(
            long,
            required_unless_present = "synthetic",
            conflicts_with = "synthetic"
        )]
        image: Option<PathBuf>,
        #[arg(
            long,
            required_unless_present = "synthetic",
            conflicts_with = "synthetic"
        )]
        alto: Option<PathBuf>,
        /// Use the built-in synthetic page (two lines of block capitals) instead of --image and --alto.
        #[arg(long)]
        synthetic: bool,
        #[arg(long)]
        dir: PathBuf,
        #[arg(
            long,
            default_value = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        )]
        engine_image: String,
        #[arg(long, env = "USNM_QWEN_LLAMA_IMAGE")]
        engine_llama: String,
        #[arg(long, env = "USNM_QWEN_WEIGHTS")]
        engine_weights: String,
        #[arg(long, default_value_t = 522)]
        crop_seed: u64,
        #[arg(long, default_value_t = 12)]
        k_crops: u32,
        #[arg(long, default_value_t = 1800)]
        timeout_secs: u32,
        #[arg(long, default_value_t = 4)]
        max_tokens_per_1000px2: u32,
    },
    /// Print llama-server's version and the model files in the image, without loading a model.
    Probe,
    /// script_validator's init script: check one result (three files); --keep copies it there.
    Check {
        #[arg(long, num_args = 3, required = true)]
        files: Vec<PathBuf>,
        #[arg(long)]
        keep: Option<PathBuf>,
    },
    /// Every bundle page under --root with its own pair verdict (JSON Lines).
    PageLedger {
        #[arg(long)]
        root: PathBuf,
        /// The directory `compare --pages` writes to.
        #[arg(long)]
        pages: PathBuf,
    },
    /// script_validator's compare script: two results of one unit; --log appends the pair.
    Compare {
        #[arg(long, num_args = 3, required = true)]
        files: Vec<PathBuf>,
        #[arg(long, num_args = 3, required = true)]
        files2: Vec<PathBuf>,
        #[arg(long, default_value_t = usnm_ocr_contract::qwen::PROBE_MIN_DICE)]
        min_dice: f64,
        #[arg(long)]
        log: Option<PathBuf>,
        /// Keep each bundle page's verdict and both readings of it here.
        #[arg(long)]
        pages: Option<PathBuf>,
        /// The rule that decides the pair. `strict-v1` is the rule in force.
        #[arg(long, value_enum, default_value_t = super::validate::Rule::StrictV1)]
        rule: super::validate::Rule,
        /// Under `crop-consensus-v2`, keep each pair's crop states here.
        #[arg(long)]
        crop_states: Option<PathBuf>,
    },
    /// Measure the crop-consensus-v2 floor on crops with human transcriptions, offline.
    Grounded {
        /// JSON Lines of `{id, source, lang, ref}`.
        #[arg(long)]
        refs: PathBuf,
        /// `NAME=PATH`, JSON Lines of `{id, text}`; repeatable.
        #[arg(long = "reader", required = true, value_parser = named_path)]
        readers: Vec<(String, PathBuf)>,
        /// `A:B`, two reader names; repeatable.
        #[arg(long = "pair", required = true, value_parser = reader_pair)]
        pairs: Vec<(String, String)>,
        /// A reader whose error on each crop bands it `clean`, `fair` or `poor`.
        #[arg(long)]
        legibility: Option<PathBuf>,
        #[arg(long, default_value_t = 0.10)]
        good_cer: f64,
        #[arg(long, default_value_t = 0.50)]
        bad_cer: f64,
        /// Another floor for the rule (and the Dice it is compared with).
        #[arg(long, default_value_t = super::consensus::Params::IN_RULE.floor)]
        floor: f64,
        #[arg(long, default_value_t = 1434)]
        seed: u64,
        #[arg(long)]
        out: PathBuf,
        /// Also write one JSON line per crop and pair here.
        #[arg(long)]
        crops: Option<PathBuf>,
    },
    /// Measure validator rules against retained results, offline, with negative controls.
    Replay {
        /// The directory `check --keep` writes to.
        #[arg(long)]
        seen: PathBuf,
        #[arg(long, default_value = "qwen3-vl-8b-q4km")]
        model: String,
        #[arg(long, default_value_t = 1420)]
        seed: u64,
        /// Random cross-page pairs and random crop pairs per control.
        #[arg(long, default_value_t = 2000)]
        controls: usize,
        #[arg(long)]
        out: PathBuf,
    },
    /// Print a Markdown table of the kept results and the logged pairs.
    Summary {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        seen: PathBuf,
        #[arg(long)]
        pairs: PathBuf,
    },
}

pub async fn run(command: QwenCommands) -> anyhow::Result<()> {
    use crate::qwen as x;
    match command {
        QwenCommands::Candidates {
            job,
            page,
            candidate,
            out,
        } => {
            x::candidates::run(&job, &page, &candidate, &out)?;
        }
        QwenCommands::BenchmarkStage {
            crops,
            root,
            engine,
            max_crops,
            timeout_secs,
            doc_ids,
        } => {
            let engine = serde_json::from_slice(&std::fs::read(engine)?)?;
            let staged = x::benchmark::stage(&x::benchmark::Options {
                crops,
                root,
                engine,
                max_crops,
                timeout_secs,
                doc_ids,
            })?;
            eprintln!("{staged} benchmark units staged");
        }
        QwenCommands::BenchmarkCollect { root, spool, out } => {
            let summary = x::benchmark::collect(&root, &spool, &out)?;
            eprintln!("{}", serde_json::to_string(&summary)?);
        }
        QwenCommands::Stage {
            quality_root,
            members,
            root,
            engine_image,
            engine_llama,
            engine_weights,
            gpu_timeout_secs,
            cpu_timeout_secs,
            max_tokens_per_1000px2,
            prompt_version,
            max_pages,
            bundle_size,
        } => {
            let s = x::stage::run_selected(
                &x::stage::Options {
                    quality_root,
                    root,
                    engine: usnm_ocr_contract::qwen::Engine {
                        image: engine_image,
                        llama: engine_llama,
                        models: x::default_models(),
                        weights: engine_weights,
                    },
                    gpu_timeout_secs,
                    cpu_timeout_secs,
                    max_tokens_per_1000px2,
                    prompt_version,
                    max_pages,
                    bundle_size,
                },
                members.as_deref(),
            )
            .await?;
            eprintln!("{s:?}");
        }
        QwenCommands::Submit {
            members,
            root,
            project,
            max_units,
            target,
            model_dir,
        } => {
            let s = x::submit::run_selected(
                &x::submit::Options {
                    root,
                    project,
                    max_units,
                    target,
                    model_dir,
                },
                members.as_deref(),
            )
            .await?;
            eprintln!("{s:?}");
        }
        QwenCommands::LocalUnit {
            image,
            alto,
            synthetic,
            dir,
            engine_image,
            engine_llama,
            engine_weights,
            crop_seed,
            k_crops,
            timeout_secs,
            max_tokens_per_1000px2,
        } => {
            let engine = usnm_ocr_contract::qwen::Engine {
                image: engine_image,
                llama: engine_llama,
                models: x::default_models(),
                weights: engine_weights,
            };
            let j = if synthetic {
                x::stage::synthetic_unit(
                    &dir,
                    engine,
                    k_crops,
                    timeout_secs,
                    max_tokens_per_1000px2,
                )?
            } else {
                x::stage::local_unit(
                    &image.context("--image")?,
                    &alto.context("--alto")?,
                    &dir,
                    engine,
                    crop_seed,
                    k_crops,
                    timeout_secs,
                    max_tokens_per_1000px2,
                )?
            };
            println!("{}", j.id);
        }
        QwenCommands::Probe => print!("{}", x::work::probe()),
        QwenCommands::Check { files, keep } => x::validate::check(&files, keep.as_deref())?,
        QwenCommands::Compare {
            files,
            files2,
            min_dice,
            log,
            pages,
            rule,
            crop_states,
        } => x::validate::compare_with(
            &files,
            &files2,
            x::validate::Compare {
                min_dice,
                rule,
                log: log.as_deref(),
                pages: pages.as_deref(),
                crop_states: crop_states.as_deref(),
            },
        )?,
        QwenCommands::Grounded {
            refs,
            readers,
            pairs,
            legibility,
            good_cer,
            bad_cer,
            floor,
            seed,
            out,
            crops,
        } => {
            let got = x::grounded::run(&x::grounded::Options {
                refs: &refs,
                readers: &readers,
                pairs: &pairs,
                legibility: legibility.as_deref(),
                good_cer,
                bad_cer,
                floor,
                seed,
                out: &out,
                crops: crops.as_deref(),
            })?;
            print!("{}", x::grounded::table(&got));
        }
        QwenCommands::Replay {
            seen,
            model,
            seed,
            controls,
            out,
        } => x::replay::run(&seen, &model, seed, controls, &out)?,
        QwenCommands::PageLedger { root, pages } => {
            for p in x::ledger::run(&root, &pages)? {
                println!("{}", serde_json::to_string(&p)?);
            }
        }
        QwenCommands::Summary { root, seen, pairs } => {
            print!("{}", x::summary::run(&root, &seen, &pairs)?)
        }
    }
    Ok(())
}

fn named_path(s: &str) -> Result<(String, PathBuf), String> {
    let (name, path) = s.split_once('=').ok_or("expected NAME=PATH")?;
    if name.is_empty() || path.is_empty() {
        return Err("expected NAME=PATH".into());
    }
    Ok((name.to_owned(), PathBuf::from(path)))
}

fn reader_pair(s: &str) -> Result<(String, String), String> {
    let (a, b) = s.split_once(':').ok_or("expected A:B")?;
    if a.is_empty() || b.is_empty() {
        return Err("expected A:B".into());
    }
    Ok((a.to_owned(), b.to_owned()))
}
