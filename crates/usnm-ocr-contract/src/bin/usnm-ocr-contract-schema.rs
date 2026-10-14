//! Writes the engine contract's JSON Schemas (13 §13.4.8):
//! `usnm-ocr-contract-schema --out-dir ocr/contract/v1`. A test in
//! `usnm-ocr-contract` fails when the files in the repository differ from
//! what this writes.

use std::path::PathBuf;

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let mut out: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match (a.as_str(), args.next()) {
            ("--out-dir", Some(d)) => out = Some(d.into()),
            _ => {
                eprintln!("usage: usnm-ocr-contract-schema --out-dir <dir>");
                return std::process::ExitCode::from(2);
            }
        }
    }
    let Some(out) = out else {
        eprintln!("usage: usnm-ocr-contract-schema --out-dir <dir>");
        return std::process::ExitCode::from(2);
    };
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("{}: {e}", out.display());
        return std::process::ExitCode::FAILURE;
    }
    for (name, schema) in usnm_ocr_contract::engine::schemas() {
        let path = out.join(format!("{name}.schema.json"));
        if let Err(e) = std::fs::write(&path, schema) {
            eprintln!("{}: {e}", path.display());
            return std::process::ExitCode::FAILURE;
        }
        println!("wrote {}", path.display());
    }
    std::process::ExitCode::SUCCESS
}
