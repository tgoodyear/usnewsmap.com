//! Validator for BOINC's `script_validator`.
//!
//! BOINC runs `../bin/usnm-boinc-validate <files>` for the init step and
//! `../bin/usnm-boinc-validate <files> <files2>` for the compare step, with output file paths
//! as positional arguments. It cannot pass flags; that is BOINC's calling convention, not a
//! choice here. Run by hand, the named form works too:
//!
//!   usnm-boinc-validate --init FILE [--config FILE]
//!   usnm-boinc-validate --compare FILE_A --with FILE_B [--config FILE]
//!
//! The pins and thresholds come from the JSON file named by `--config`, or by the environment
//! variable `USNM_BOINC_VALIDATOR_CONFIG`, or the built-in pilot defaults.
//!
//! Exit status: 0 valid or equivalent, 3 transient (retry later), 1 invalid.

use std::process::ExitCode;
use usnm_boinc::cli::Flags;
use usnm_boinc::result::{check, compare, parse, Comparison, Config, ResultFile};

const EXIT_INVALID: u8 = 1;
const EXIT_TRANSIENT: u8 = 3;

fn load_config(path: Option<String>) -> Result<Config, String> {
    let path = path.or_else(|| std::env::var("USNM_BOINC_VALIDATOR_CONFIG").ok());
    match path {
        None => Ok(Config::pilot()),
        Some(p) => {
            let text = std::fs::read(&p).map_err(|e| format!("config {p}: {e}"))?;
            serde_json::from_slice(&text).map_err(|e| format!("config {p}: {e}"))
        }
    }
}

enum Load {
    Transient(String),
    Invalid(String),
}

fn load(path: &str, cfg: &Config) -> Result<ResultFile, Load> {
    let bytes = std::fs::read(path).map_err(|e| Load::Transient(format!("{path}: {e}")))?;
    let r = parse(&bytes).map_err(|e| Load::Invalid(format!("{path}: {e}")))?;
    check(&r, cfg).map_err(|e| Load::Invalid(format!("{path}: {e}")))?;
    Ok(r)
}

fn run(args: Vec<String>) -> Result<(), (u8, String)> {
    let (init, cmp, config): (Option<String>, Option<(String, String)>, Option<String>) =
        if args.first().is_some_and(|a| a.starts_with("--")) {
            let f = Flags::parse(&args, &[]).map_err(|e| (EXIT_INVALID, e))?;
            let cmp = match (f.get("compare"), f.get("with")) {
                (Some(a), Some(b)) => Some((a.to_string(), b.to_string())),
                (None, None) => None,
                _ => return Err((EXIT_INVALID, "--compare needs --with".into())),
            };
            (
                f.get("init").map(str::to_string),
                cmp,
                f.get("config").map(str::to_string),
            )
        } else {
            match args.as_slice() {
                [a] => (Some(a.clone()), None, None),
                [a, b] => (None, Some((a.clone(), b.clone())), None),
                _ => return Err((EXIT_INVALID, "expected one or two result files".into())),
            }
        };
    let cfg = load_config(config).map_err(|e| (EXIT_TRANSIENT, e))?;
    let conv = |l: Load| match l {
        Load::Transient(m) => (EXIT_TRANSIENT, m),
        Load::Invalid(m) => (EXIT_INVALID, m),
    };
    if let Some(p) = init {
        load(&p, &cfg).map(|_| ()).map_err(conv)
    } else if let Some((a, b)) = cmp {
        let ra = load(&a, &cfg).map_err(conv)?;
        let rb = load(&b, &cfg).map_err(conv)?;
        match compare(&ra, &rb, &cfg) {
            Comparison::Match => Ok(()),
            Comparison::Mismatch(m) => Err((EXIT_INVALID, format!("results differ: {m}"))),
        }
    } else {
        Err((EXIT_INVALID, "nothing to do".into()))
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, msg)) => {
            eprintln!("usnm-boinc-validate: {msg}");
            ExitCode::from(code)
        }
    }
}
