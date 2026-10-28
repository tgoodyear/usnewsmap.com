//! The `usnm-boinc quality` command line (#522): its clap definitions and the dispatch to
//! this module's stages. `main.rs` only names the group.
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum QualityCommands {
    /// Draw the campaign's pages from the prefetch manifest, stratified as the assessment's sample.
    Sample {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 500)]
        pages: usize,
        #[arg(long, default_value_t = 2)]
        min_per_stratum: usize,
        #[arg(long)]
        seed: String,
        /// JSON object: LCCN to the title's MARC language codes.
        #[arg(long)]
        title_languages: Option<PathBuf>,
        /// Doc ids that have an American Stories text, one per line; pages not in it are left out.
        #[arg(long)]
        with_as: Option<PathBuf>,
        /// Explicit doc ids, one per line, instead of a random draw.
        #[arg(long)]
        pages_file: Option<PathBuf>,
    },
    /// Export each page's non-LoC texts (American Stories, ocr-local) and our ALTO from the curated store.
    TextsExport {
        #[arg(long)]
        curated_url: String,
        /// Read the American Stories page shards from here (a local copy of `curated`, or
        /// another store URL) instead of --curated-url; the ocr-local overlays still come from
        /// --curated-url.
        #[arg(long)]
        american_stories_url: Option<String>,
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long)]
        sample: Option<PathBuf>,
        #[arg(long = "doc-id")]
        doc_ids: Vec<String>,
    },
    /// Write <lang>.txt function-word files from the embedded word lists.
    FunctionWords {
        #[arg(long = "lang", required = true)]
        langs: Vec<String>,
        #[arg(long, default_value_t = 100)]
        top: usize,
        #[arg(long)]
        out_dir: PathBuf,
    },
    /// Fetch each page's JPEG and ALTO from LoC (or read the hand-off scans), join its texts, write its unit.
    Stage {
        /// Repeatable page spec: group:lang:doc_id:batch[+batch...] (batches as <name>_ver<NN>).
        #[arg(long = "page")]
        pages: Vec<crate::surya::Page>,
        /// Or a sample (quality sample): its rows become pages of --group.
        #[arg(long)]
        sample: Option<PathBuf>,
        #[arg(long, default_value = "sample")]
        group: String,
        #[arg(long)]
        root: PathBuf,
        /// The worker image's registry digest (sha256:...).
        #[arg(long)]
        engine_image: String,
        /// The hand-off job's texts/ export: one <doc_id>.json per page.
        #[arg(long)]
        texts_dir: PathBuf,
        /// `surya collect`'s export (<doc_id>.surya.json): the page's Surya text joins the unit as
        /// the reader ours-surya when --readers names it (#968).
        #[arg(long)]
        surya_texts: Option<PathBuf>,
        /// The hand-off container's scans/ (<doc_id>.jpg, <doc_id>.xml), read instead of LoC when present.
        #[arg(long)]
        scans_dir: Option<PathBuf>,
        /// The campaign's place names: a .txt (one per line) or the catalog's places.json.
        #[arg(long)]
        places: PathBuf,
        /// Directory of <lang>.txt function-word files, one word per line.
        #[arg(long, alias = "function-words")]
        function_words_dir: PathBuf,
        /// The readers every unit compares, in report order.
        #[arg(long, value_delimiter = ',', default_value = "loc,as")]
        readers: Vec<String>,
        #[arg(long, default_value_t = 522)]
        crop_seed: u64,
        #[arg(long, default_value_t = 12)]
        k_crops: u32,
        #[arg(long, default_value_t = 5)]
        gap_secs: u64,
        #[arg(long, default_value_t = 20)]
        max_pages: usize,
        /// Pack staged pages into multi-page bundle workunits of up to this many pages (1 to 20).
        #[arg(long, default_value_t = 1)]
        bundle_size: usize,
        /// The scan archive's container URL (https://<account>.blob.core.windows.net/scans); read first with the job's identity.
        #[arg(long)]
        scan_archive: Option<String>,
        /// Where batch manifests and METS are kept between runs.
        #[arg(long)]
        batch_cache: Option<PathBuf>,
    },
    /// Hand staged units to create_work, two results each.
    Submit {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        #[arg(long, default_value_t = 100)]
        max_units: usize,
    },
    /// Write a unit from local files, to check a host or the image without LoC or BOINC.
    LocalUnit {
        #[arg(long)]
        page: PathBuf,
        #[arg(long)]
        alto: PathBuf,
        /// texts.json.
        #[arg(long)]
        texts: PathBuf,
        #[arg(long)]
        places: PathBuf,
        #[arg(long)]
        function_words: PathBuf,
        #[arg(long)]
        dir: PathBuf,
        #[arg(
            long,
            default_value = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        )]
        engine_image: String,
        #[arg(long, default_value_t = 522)]
        crop_seed: u64,
        #[arg(long, default_value_t = 12)]
        k_crops: u32,
    },
    /// script_validator's init script: check one result (three files); --keep copies it there.
    Check {
        #[arg(long, num_args = 3, required = true)]
        files: Vec<PathBuf>,
        #[arg(long)]
        keep: Option<PathBuf>,
    },
    /// script_validator's compare script: two results of one unit; --log appends the pair.
    Compare {
        #[arg(long, num_args = 3, required = true)]
        files: Vec<PathBuf>,
        #[arg(long, num_args = 3, required = true)]
        files2: Vec<PathBuf>,
        #[arg(long)]
        log: Option<PathBuf>,
    },
    /// Join both apps' canonical results per page into the campaign's Parquet tables and summary.
    Collect {
        #[arg(long)]
        campaign: String,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        qwen_root: PathBuf,
        #[arg(long)]
        qwen_spool: PathBuf,
        /// Reuse content-addressed per-unit rows; late canonical readings invalidate joins.
        #[arg(long)]
        cache: Option<PathBuf>,
        /// Bound recomputation per pass; incomplete cache warming never publishes partial tables.
        #[arg(long, default_value_t = 8)]
        max_dirty_units: usize,
        /// Emit bounded canonical dependency fingerprints only; no row computation or publication.
        #[arg(long, requires = "cache")]
        inventory_only: bool,
        /// Recheck just this unit's dependency key after a scoped collection.
        #[arg(long, requires = "inventory_only")]
        inventory_unit: Option<String>,
        /// The usnm_surya staging root and spool: its crop units' texts join crops.parquet as the
        /// reader `surya` (#968).
        #[arg(long, requires = "surya_spool")]
        surya_root: Option<PathBuf>,
        #[arg(long, requires = "surya_root")]
        surya_spool: Option<PathBuf>,
        /// A local directory instead of the curated store.
        #[arg(
            long,
            required_unless_present = "curated_url",
            conflicts_with = "curated_url"
        )]
        out_dir: Option<PathBuf>,
        #[arg(long)]
        curated_url: Option<String>,
    },
    /// Run the estimator on gold crops with a transcription and report its error per reader.
    Calibrate {
        /// Gold-style crops.jsonl with transcriptions.
        #[arg(long)]
        crops: PathBuf,
        /// ENGINE=FILE.jsonl (rows {"crop_id", "text"}); repeat.
        #[arg(long = "hyp", required = true)]
        hyps: Vec<String>,
        /// Vision-language readings per crop (rows {"crop_id", "text"}).
        #[arg(long)]
        readings: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Run the unit's computation offline on <dir>/<doc_id>/{page.jpg, loc.xml, texts.json}.
    Estimate {
        #[arg(long)]
        dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        places: PathBuf,
        #[arg(long)]
        function_words: PathBuf,
        #[arg(long, default_value_t = 522)]
        crop_seed: u64,
        #[arg(long, default_value_t = 12)]
        k_crops: u32,
        /// Survey layout (<dir>/sample/images): NAME=PATTERN, the * standing for the page name; repeat.
        #[arg(long = "reader")]
        readers: Vec<String>,
    },
    /// Recompute the page hat, version 1 (whole-page cer_sym) and version 2 (passage-matched),
    /// from a staging root's texts; one JSON line per page and reader (#962).
    Rehat {
        /// The campaign's staging root (holds units/).
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 3)]
        min_readers: usize,
        #[arg(long, default_value_t = 1)]
        threads: usize,
    },
    /// Compute single-page units' text part again from corrected LoC texts, keeping their scan
    /// features: the re-staged units and pages.parquet, with no new volunteer work (#1079).
    Rescore {
        #[arg(long)]
        campaign: String,
        /// The campaign's staging root (holds units/).
        #[arg(long)]
        root: PathBuf,
        /// Where the assimilator keeps the canonical results.
        #[arg(long)]
        spool: PathBuf,
        /// JSON lines {"doc_id", "text"}: LoC's text of each page, for the loc-hist reader.
        #[arg(long)]
        loc_texts: PathBuf,
        /// The campaign's places.txt.
        #[arg(long)]
        places: PathBuf,
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long, default_value_t = 1)]
        threads: usize,
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

pub async fn run(command: QualityCommands) -> anyhow::Result<()> {
    use crate::quality as x;
    match command {
        QualityCommands::Sample {
            manifest,
            out,
            pages,
            min_per_stratum,
            seed,
            title_languages,
            with_as,
            pages_file,
        } => {
            let s = x::sample::run(&x::sample::Options {
                manifest,
                out,
                pages,
                min_per_stratum,
                seed,
                title_languages,
                with_as,
                pages_file,
            })?;
            eprintln!("{s:?}");
        }
        QualityCommands::TextsExport {
            curated_url,
            american_stories_url,
            out_dir,
            sample,
            doc_ids,
        } => {
            let s = x::stage::texts_export(&x::stage::TextsExportOptions {
                curated_url,
                american_stories_url,
                out_dir,
                sample,
                doc_ids,
            })
            .await?;
            eprintln!("{s:?}");
        }
        QualityCommands::FunctionWords {
            langs,
            top,
            out_dir,
        } => {
            x::stage::write_function_words(&langs, top, &out_dir)?;
        }
        QualityCommands::Stage {
            pages,
            sample,
            group,
            root,
            engine_image,
            texts_dir,
            surya_texts,
            scans_dir,
            places,
            function_words_dir,
            readers,
            crop_seed,
            k_crops,
            gap_secs,
            max_pages,
            bundle_size,
            scan_archive,
            batch_cache,
        } => {
            let s = x::stage::run(&x::stage::Options {
                pages,
                sample,
                group,
                root,
                engine: usnm_ocr_contract::quality::Engine {
                    image: engine_image,
                },
                texts_dir,
                surya_texts,
                scans_dir,
                places,
                function_words_dir,
                readers,
                crop_seed,
                k_crops,
                gap: std::time::Duration::from_secs(gap_secs),
                max_pages,
                bundle_size,
                scan_archive,
                batch_cache,
            })
            .await?;
            eprintln!("{s:?}");
        }
        QualityCommands::Submit {
            root,
            project,
            max_units,
        } => {
            let s = x::submit::run(&x::submit::Options {
                root,
                project,
                max_units,
            })
            .await?;
            eprintln!("{s:?}");
        }
        QualityCommands::LocalUnit {
            page,
            alto,
            texts,
            places,
            function_words,
            dir,
            engine_image,
            crop_seed,
            k_crops,
        } => {
            let texts: usnm_ocr_contract::quality::Texts = serde_json::from_slice(&crate::read(
                &texts,
                usnm_ocr_contract::quality::MAX_TEXTS,
            )?)?;
            let j = x::stage::local_unit(&x::stage::LocalUnit {
                page: &crate::read(&page, usnm_ocr_contract::quality::MAX_SCAN)?,
                alto: &crate::read(&alto, usnm_ocr_contract::quality::MAX_ALTO)?,
                texts: &texts,
                places: &std::fs::read_to_string(&places)?,
                dir: &dir,
                engine: usnm_ocr_contract::quality::Engine {
                    image: engine_image,
                },
                function_words: x::stage::read_words(&function_words)?,
                crop_seed,
                k_crops,
            })?;
            println!("{}", j.id);
        }
        QualityCommands::Check { files, keep } => x::validate::check(&files, keep.as_deref())?,
        QualityCommands::Compare { files, files2, log } => {
            x::validate::compare(&files, &files2, log.as_deref())?
        }
        QualityCommands::Collect {
            campaign,
            root,
            spool,
            qwen_root,
            qwen_spool,
            cache,
            max_dirty_units,
            inventory_only,
            inventory_unit,
            surya_root,
            surya_spool,
            out_dir,
            curated_url,
        } => {
            let opts = x::collect::Options {
                campaign,
                root,
                spool,
                qwen_root,
                qwen_spool,
                cache,
                max_dirty_units,
                surya_root,
                surya_spool,
                out_dir,
                curated_url,
            };
            if inventory_only {
                println!(
                    "{}",
                    serde_json::to_string(&x::collect::inventory(
                        &opts,
                        inventory_unit.as_deref()
                    )?)?
                );
            } else {
                let s = x::collect::run(&opts).await?;
                eprintln!("{s:?}");
            }
        }
        QualityCommands::Calibrate {
            crops,
            hyps,
            readings,
            out,
        } => {
            let rows = x::calibrate::run(&x::calibrate::Options {
                crops,
                hyps,
                readings,
                out,
            })?;
            for r in rows {
                println!("{}", serde_json::to_string(&r)?);
            }
        }
        QualityCommands::Estimate {
            dir,
            out,
            places,
            function_words,
            crop_seed,
            k_crops,
            readers,
        } => {
            let s = x::estimate::run(&x::estimate::Options {
                dir,
                out,
                places,
                function_words,
                crop_seed,
                k_crops,
                readers,
            })?;
            eprintln!("{s:?}");
        }
        QualityCommands::Rehat {
            root,
            out,
            min_readers,
            threads,
        } => {
            let s = x::rehat::run(&x::rehat::Options {
                root,
                out,
                min_readers,
                threads,
            })?;
            eprintln!("{s:?}");
        }
        QualityCommands::Rescore {
            campaign,
            root,
            spool,
            loc_texts,
            places,
            out_dir,
            threads,
        } => {
            let s = x::rescore::run(&x::rescore::Options {
                campaign,
                root,
                spool,
                loc_texts,
                places,
                out_dir,
                threads,
            })?;
            eprintln!("{s:?}");
        }
        QualityCommands::Summary { root, seen, pairs } => {
            print!("{}", x::summary::run(&root, &seen, &pairs)?)
        }
    }
    Ok(())
}
