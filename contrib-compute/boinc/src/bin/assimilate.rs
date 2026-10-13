//! Assimilator for BOINC's `script_assimilator`.
//!
//! BOINC runs `../bin/usnm-boinc-assimilate <wu_name> <wu_id> <result_id> <files>` when a
//! workunit has a canonical result (positional, BOINC's convention), and
//! `usnm-boinc-assimilate --error N <wu_name> <wu_id> <batch_id>` when it failed.
//! Configure the daemon with `--script "usnm-boinc-assimilate wu_name wu_id result_id files"`.
//!
//! Named form for manual runs:
//!
//!   usnm-boinc-assimilate --result FILE --result-id N --out-dir DIR \
//!       --host-id N --user-id N --app-version TEXT --credit X --agreeing N [--trusted]
//!
//! The real sink and the real BOINC database lookup are TODO stubs that refuse to run; see
//! `usnm_boinc::assimilate`. `--out-dir` writes parts to a local directory instead.

use std::process::ExitCode;
use usnm_boinc::assimilate::{
    assimilate, LocalDirSink, ProvenanceSource, ResultMeta, UnimplementedProvenance,
    UnimplementedSink,
};
use usnm_boinc::cli::Flags;
use usnm_boinc::result::{check, parse, Config};

struct Manual(ResultMeta);
impl ProvenanceSource for Manual {
    fn lookup(&self, _id: u64) -> Result<ResultMeta, String> {
        Ok(self.0.clone())
    }
}

fn run(args: Vec<String>) -> Result<String, String> {
    if args.first().map(String::as_str) == Some("--error") {
        // Failed workunit: nothing to assimilate. The pages go back to the manifest, and the
        // work generator's state file lets them be sliced again once the entry is removed.
        return Ok(format!("workunit failed: {}", args[1..].join(" ")));
    }
    let (result_path, result_id, flags) = if args.first().is_some_and(|a| a.starts_with("--")) {
        let f = Flags::parse(&args, &["trusted"])?;
        let id: u64 = f.get_parsed("result-id", 0)?;
        (f.require("result")?.to_string(), id, Some(f))
    } else if let [_wu_name, _wu_id, result_id, file] = args.as_slice() {
        (
            file.clone(),
            result_id.parse().map_err(|_| "bad result_id".to_string())?,
            None,
        )
    } else {
        return Err("expected <wu_name> <wu_id> <result_id> <file>".into());
    };

    let bytes = std::fs::read(&result_path).map_err(|e| format!("{result_path}: {e}"))?;
    let r = parse(&bytes).map_err(|e| e.to_string())?;
    // The validator already checked this. Check again: the assimilator is the last gate
    // before the intake.
    check(&r, &Config::pilot()).map_err(|e| e.to_string())?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let out_dir = flags.as_ref().and_then(|f| f.get("out-dir"));
    let n = match (&flags, out_dir) {
        (Some(f), Some(dir)) => {
            let meta = ResultMeta {
                app_version: f.require("app-version")?.to_string(),
                credit: f.get_parsed("credit", 0.0)?,
                host_id: f.get_parsed("host-id", 0)?,
                n_agreeing: f.get_parsed("agreeing", 1)?,
                result_id,
                trusted_host: f.has("trusted"),
                user_id: f.get_parsed("user-id", 0)?,
            };
            let mut sink = LocalDirSink { root: dir.into() };
            assimilate(&r, &Manual(meta), result_id, now, &mut sink)?
        }
        _ => assimilate(
            &r,
            &UnimplementedProvenance,
            result_id,
            now,
            &mut UnimplementedSink,
        )?,
    };
    Ok(format!("assimilated {n} pages from result {result_id}"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args) {
        Ok(msg) => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("usnm-boinc-assimilate: {e}");
            ExitCode::FAILURE
        }
    }
}
