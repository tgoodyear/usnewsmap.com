//! The `usnm-boinc surya` command line (#516): its clap definitions and the dispatch to
//! this module's stages. `main.rs` only names the group.
use clap::{Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum SuryaCommands {
    /// Stage checksummed benchmark images and private references, with no network requests.
    BenchmarkStage {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        manifest_sha256: String,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        engine_image: String,
        #[arg(long)]
        engine_llama: String,
        #[arg(long)]
        engine_weights: String,
        #[arg(long, default_value = "0.22.1")]
        engine_surya: String,
        /// CPU modes for an apples-to-apples bounded probe; defaults remain tiled only.
        #[arg(long, value_delimiter = ',', default_value = "tiled")]
        cpu_modes: Vec<SuryaMode>,
    },

    /// Get each page's full-size IIIF JPEG (the scan archive, then tile.loc.gov one request at a
    /// time) and write its unit.
    Stage {
        /// group:lang:doc_id:batch[+batch...], e.g.
        /// serbian:srp:sn85055101_1917-04-03_ed-1_seq-3:dlc_arlington_ver01; repeat.
        #[arg(long = "page", required_unless_present = "sample")]
        pages: Vec<crate::surya::Page>,
        /// Or the quality campaign's sample (`quality sample`, JSON Lines): its rows become
        /// pages of --group, in their batch, and each unit keeps its row as sample.json.
        #[arg(long, requires = "group")]
        sample: Option<PathBuf>,
        /// The group the sample's pages are staged as (the quality campaign's, `campaign-1` say).
        #[arg(long)]
        group: Option<String>,
        #[arg(long)]
        root: PathBuf,
        /// The worker image's registry digest (sha256:...).
        #[arg(long)]
        engine_image: String,
        /// The llama.cpp server image it is built on (its USNM_SURYA_LLAMA_IMAGE).
        #[arg(long)]
        engine_llama: String,
        /// Its USNM_SURYA_WEIGHTS.
        #[arg(long)]
        engine_weights: String,
        #[arg(long, default_value = "0.22.1")]
        engine_surya: String,
        #[arg(long, value_delimiter = ',', default_value = "tiled,whole,capped")]
        gpu_modes: Vec<SuryaMode>,
        #[arg(long, value_delimiter = ',', default_value = "tiled")]
        cpu_modes: Vec<SuryaMode>,
        #[arg(long, default_value_t = 1800)]
        gpu_timeout_secs: u32,
        #[arg(long, default_value_t = 5400)]
        cpu_timeout_secs: u32,
        #[arg(long, default_value_t = 5)]
        gap_secs: u64,
        #[arg(long, default_value_t = 20)]
        max_pages: usize,
        #[arg(long)]
        batch_cache: Option<PathBuf>,
        /// The scan archive's container URL, read first; omit for LoC only.
        #[arg(long)]
        scan_archive: Option<String>,
    },
    /// Write one unit per crop of every staged usnm_quality page (#968): the crops the page's
    /// usnm_qwen unit reads, cut here from its page.jpg, read by Surya in whole mode.
    StageCrops {
        /// The usnm_quality staging root (its pages/ or units/ hold page.jpg, loc.xml and
        /// quality.json).
        #[arg(long)]
        quality_root: PathBuf,
        #[arg(long)]
        root: PathBuf,
        /// The worker image's registry digest (sha256:...).
        #[arg(long)]
        engine_image: String,
        #[arg(long)]
        engine_llama: String,
        #[arg(long)]
        engine_weights: String,
        #[arg(long, default_value = "0.22.1")]
        engine_surya: String,
        /// Deadline for the one whole-mode read, on a GPU and on a CPU.
        #[arg(long, default_value_t = 600)]
        timeout_secs: u32,
        /// Pages to take new units from, at most.
        #[arg(long, default_value_t = 50)]
        max_pages: usize,
    },
    /// Write one unit per crop of the OCR gold set's manifest (#968): the boxes a person
    /// transcribes, cut from each row's page (fetched from tile.loc.gov one request at a time, or
    /// read from --pages-dir) and read by Surya in whole mode.
    StageGold {
        /// The gold manifest (fixtures/ocr-gold/<set>/manifest.jsonl).
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        root: PathBuf,
        /// The worker image's registry digest (sha256:...).
        #[arg(long)]
        engine_image: String,
        #[arg(long)]
        engine_llama: String,
        #[arg(long)]
        engine_weights: String,
        #[arg(long, default_value = "0.22.1")]
        engine_surya: String,
        /// Deadline for the one whole-mode read, on a GPU and on a CPU.
        #[arg(long, default_value_t = 600)]
        timeout_secs: u32,
        /// Crops to stage, at most.
        #[arg(long, default_value_t = 50)]
        max_crops: usize,
        /// Seconds between two requests to tile.loc.gov (5 or more).
        #[arg(long, default_value_t = 5)]
        gap_secs: u64,
        /// Read <dir>/<page_id>.jpg instead of fetching the page.
        #[arg(long)]
        pages_dir: Option<PathBuf>,
    },
    /// Write Surya's reading of the gold crops as JSON Lines {crop_id, text}, the file
    /// `gold.py score --hyp surya=FILE` reads (#968).
    GoldHyp {
        #[arg(long)]
        root: PathBuf,
        /// The assimilator's results directory (<workunit>__file_<name>).
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Hand staged units to create_work, two results each.
    Submit {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        #[arg(long, default_value_t = 20)]
        max_units: usize,
        /// Experimental single-replica probe, at most 30 units, no fallback hosts.
        #[arg(long)]
        target_host: Option<u64>,
        /// Exact-host NVIDIA version 1.3 only, with its 8 GiB memory envelope.
        #[arg(long, requires = "target_host")]
        gpu_probe: bool,
    },
    /// Write a unit from a local image, to check a host or the image without LoC or BOINC.
    /// Inside the worker image the engine values default to the image's own.
    LocalUnit {
        #[arg(long)]
        image: PathBuf,
        #[arg(long)]
        dir: PathBuf,
        #[arg(
            long,
            default_value = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        )]
        engine_image: String,
        #[arg(long, env = "USNM_SURYA_LLAMA_IMAGE")]
        engine_llama: String,
        #[arg(long, env = "USNM_SURYA_WEIGHTS")]
        engine_weights: String,
        #[arg(long, env = "USNM_SURYA_VERSION", default_value = "0.22.1")]
        engine_surya: String,
        #[arg(long, value_delimiter = ',', default_value = "whole")]
        modes: Vec<SuryaMode>,
        #[arg(long, default_value_t = 1800)]
        timeout_secs: u32,
    },
    /// script_validator's init script: check one result (five files); --keep copies it there.
    Check {
        #[arg(long, num_args = 5, required = true)]
        files: Vec<PathBuf>,
        #[arg(long)]
        keep: Option<PathBuf>,
    },
    /// script_validator's compare script: two results of one unit; --log appends the pair.
    Compare {
        #[arg(long, num_args = 5, required = true)]
        files: Vec<PathBuf>,
        #[arg(long, num_args = 5, required = true)]
        files2: Vec<PathBuf>,
        #[arg(long, default_value_t = usnm_ocr_contract::surya::PROBE_MIN_DICE)]
        min_dice: f64,
        #[arg(long)]
        log: Option<PathBuf>,
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
    /// Read the campaign's canonical results from the assimilator's spool and export each page's
    /// text as the ours-surya reader for `quality stage --surya-texts` (#968).
    Collect {
        #[arg(long)]
        root: PathBuf,
        /// The assimilator's results directory (<workunit>__file_<name>).
        #[arg(long)]
        spool: PathBuf,
        /// Where <doc_id>.surya.json goes; omit to only count results.
        #[arg(long)]
        texts_dir: Option<PathBuf>,
        /// Which mode's text stands for the page, first found wins.
        #[arg(long, value_delimiter = ',', default_value = "whole,capped,tiled")]
        modes: Vec<SuryaMode>,
    },
}
#[derive(Clone, Copy, ValueEnum)]
pub enum SuryaMode {
    Tiled,
    Whole,
    Capped,
}
impl From<SuryaMode> for usnm_ocr_contract::surya::InputMode {
    fn from(m: SuryaMode) -> Self {
        match m {
            SuryaMode::Tiled => Self::Tiled,
            SuryaMode::Whole => Self::Whole,
            SuryaMode::Capped => Self::Capped,
        }
    }
}

pub async fn run(command: SuryaCommands) -> anyhow::Result<()> {
    use crate::surya as x;
    match command {
        SuryaCommands::BenchmarkStage {
            manifest,
            manifest_sha256,
            root,
            engine_image,
            engine_llama,
            engine_weights,
            engine_surya,
            cpu_modes,
        } => {
            let count = x::stage::benchmark_with_cpu_modes(
                &manifest,
                &manifest_sha256,
                &root,
                usnm_ocr_contract::surya::Engine {
                    image: engine_image,
                    llama: engine_llama,
                    weights: engine_weights,
                    surya: engine_surya,
                },
                &cpu_modes.into_iter().map(Into::into).collect::<Vec<_>>(),
            )?;
            eprintln!("staged {count} immutable benchmark inputs");
        }

        SuryaCommands::Stage {
            mut pages,
            sample,
            group,
            root,
            engine_image,
            engine_llama,
            engine_weights,
            engine_surya,
            gpu_modes,
            cpu_modes,
            gpu_timeout_secs,
            cpu_timeout_secs,
            gap_secs,
            max_pages,
            batch_cache,
            scan_archive,
        } => {
            let mut sample_rows = std::collections::BTreeMap::new();
            if let Some(sample) = &sample {
                let group = group.as_deref().unwrap_or_default();
                for row in crate::assess::read_sample(sample)? {
                    pages.push(x::Page::from_sample(&row, group)?);
                    sample_rows.insert(row.doc_id.clone(), row);
                }
            }
            let s = x::stage::run(&x::stage::Options {
                pages,
                root,
                engine: usnm_ocr_contract::surya::Engine {
                    image: engine_image,
                    llama: engine_llama,
                    surya: engine_surya,
                    weights: engine_weights,
                },
                gpu_modes: gpu_modes.into_iter().map(Into::into).collect(),
                cpu_modes: cpu_modes.into_iter().map(Into::into).collect(),
                gpu_timeout_secs,
                cpu_timeout_secs,
                gap: std::time::Duration::from_secs(gap_secs),
                max_pages,
                batch_cache,
                scan_archive,
                sample_rows,
            })
            .await?;
            eprintln!("{s:?}");
        }
        SuryaCommands::StageCrops {
            quality_root,
            root,
            engine_image,
            engine_llama,
            engine_weights,
            engine_surya,
            timeout_secs,
            max_pages,
        } => {
            let s = x::crops::run(&x::crops::Options {
                quality_root,
                root,
                engine: usnm_ocr_contract::surya::Engine {
                    image: engine_image,
                    llama: engine_llama,
                    surya: engine_surya,
                    weights: engine_weights,
                },
                timeout_secs,
                max_pages,
            })
            .await?;
            eprintln!("{s:?}");
        }
        SuryaCommands::StageGold {
            manifest,
            root,
            engine_image,
            engine_llama,
            engine_weights,
            engine_surya,
            timeout_secs,
            max_crops,
            gap_secs,
            pages_dir,
        } => {
            let s = x::gold::run(&x::gold::Options {
                manifest,
                root,
                engine: usnm_ocr_contract::surya::Engine {
                    image: engine_image,
                    llama: engine_llama,
                    surya: engine_surya,
                    weights: engine_weights,
                },
                timeout_secs,
                max_crops,
                gap: std::time::Duration::from_secs(gap_secs),
                pages_dir,
            })
            .await?;
            eprintln!("{s:?}");
        }
        SuryaCommands::GoldHyp { root, spool, out } => {
            let s = x::gold::hyp(&x::gold::HypOptions { root, spool, out })?;
            eprintln!("{} gold crops with a read, {} without", s.read, s.unread);
        }
        SuryaCommands::Submit {
            root,
            project,
            max_units,
            target_host,
            gpu_probe,
        } => {
            let s = x::submit::run(&x::submit::Options {
                root,
                project,
                max_units,
                target_host,
                gpu_probe,
            })
            .await?;
            eprintln!("{s:?}");
        }
        SuryaCommands::LocalUnit {
            image,
            dir,
            engine_image,
            engine_llama,
            engine_weights,
            engine_surya,
            modes,
            timeout_secs,
        } => {
            let j = x::stage::local_unit(
                &image,
                &dir,
                usnm_ocr_contract::surya::Engine {
                    image: engine_image,
                    llama: engine_llama,
                    surya: engine_surya,
                    weights: engine_weights,
                },
                modes.into_iter().map(Into::into).collect(),
                timeout_secs,
            )?;
            println!("{}", j.id);
        }
        SuryaCommands::Check { files, keep } => x::validate::check(&files, keep.as_deref())?,
        SuryaCommands::Compare {
            files,
            files2,
            min_dice,
            log,
        } => x::validate::compare(&files, &files2, min_dice, log.as_deref())?,
        SuryaCommands::Summary { root, seen, pairs } => {
            print!("{}", x::summary::run(&root, &seen, &pairs)?)
        }
        SuryaCommands::Collect {
            root,
            spool,
            texts_dir,
            modes,
        } => {
            let s = x::collect::run(&x::collect::Options {
                root,
                spool,
                texts_dir,
                modes: modes.into_iter().map(Into::into).collect(),
            })?;
            eprintln!("{s:?}");
        }
    }
    Ok(())
}
