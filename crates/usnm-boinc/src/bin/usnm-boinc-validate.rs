//! `script_validator` helper. BOINC runs it with the bare output-file paths
//! (no flags are possible), in the result template's order: text.txt,
//! layout.xml, result.json. Three paths check one result; six compare two
//! results. Exit 0 accepts, 1 rejects. Reasons go to stderr and never carry
//! file content.
use std::{path::Path, process::ExitCode};
use usnm_boinc::validate::{compare, load, Outputs};

fn one(p: &[String]) -> anyhow::Result<Outputs> {
    load(Path::new(&p[0]), Path::new(&p[1]), Path::new(&p[2]))
}

fn main() -> ExitCode {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    let result = match paths.len() {
        3 => one(&paths).map(|_| ()),
        6 => one(&paths[..3])
            .and_then(|a| one(&paths[3..]).and_then(|b| compare(&a, &b)))
            .map(|_| ()),
        _ => Err(anyhow::anyhow!("expected 3 paths (check) or 6 (compare)")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("usnm-boinc-validate: {e:#}");
            ExitCode::from(1)
        }
    }
}
