//! Work generator. Reads the staging manifest, slices it into workunits and calls BOINC's
//! `bin/stage_file` and `bin/create_work` in the project directory.
//!
//!   usnm-boinc-workgen --manifest FILE --project-dir DIR --state FILE
//!       [--pages-per-wu 4] [--max-per-run 50] [--max-per-day 500]
//!       [--allow-partial] [--loop-secs N] [--dry-run]
//!
//! `--mode fetch-and-ocr` reads a fetch manifest (JSON lines, see src/fetch.rs) instead, and
//! takes `--max-per-hour` and `--hold-file FILE`. While FILE exists and holds a time in the
//! future (seconds since the epoch), nothing is created: the operator's circuit breaker for
//! when the source pushes back. This mode is off by default and runs at a low per-client rate; see docs/design.md.
//!
//! With `--dry-run` it prints the commands as JSON lines and creates nothing. With
//! `--loop-secs` it repeats forever, so BOINC's `config.xml` can run it as a daemon.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use usnm_boinc::cli::Flags;
use usnm_boinc::workgen::{parse_state, select, submit, JobParams, StateEntry, Submitter};
use usnm_boinc::{fetch, manifest};

struct Shell {
    project: PathBuf,
    dry: bool,
}

impl Shell {
    fn run(&self, tool: &str, args: &[String]) -> Result<(), String> {
        if self.dry {
            let line = serde_json::json!({ "args": args, "tool": tool });
            println!("{line}");
            return Ok(());
        }
        let status = Command::new(self.project.join("bin").join(tool))
            .args(args)
            .current_dir(&self.project)
            .status()
            .map_err(|e| format!("{tool}: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("{tool} exited with {status}"))
        }
    }
}

impl Submitter for Shell {
    fn write_manifest(&mut self, file_name: &str, json: &str) -> Result<String, String> {
        let dir = std::env::temp_dir().join("usnm-boinc-workgen");
        let path = dir.join(file_name);
        if self.dry {
            return Ok(path.display().to_string());
        }
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())?;
        Ok(path.display().to_string())
    }
    fn stage_file(&mut self, args: &[String]) -> Result<(), String> {
        self.run("stage_file", args)
    }
    fn create_work(&mut self, args: &[String]) -> Result<(), String> {
        self.run("create_work", args)
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn pass_fetch(f: &Flags) -> Result<usize, String> {
    let manifest_path = f.require("manifest")?;
    let pages_per_wu: usize = f.get_parsed("pages-per-wu", 4)?;
    if pages_per_wu == 0 {
        return Err("--pages-per-wu must be positive".into());
    }
    let text =
        std::fs::read_to_string(manifest_path).map_err(|e| format!("{manifest_path}: {e}"))?;
    let pages = fetch::parse(&text).map_err(|e| e.to_string())?;
    let slices = fetch::slice(&pages, pages_per_wu, f.has("allow-partial"));
    let state_path = f.require("state")?;
    let state = std::fs::read_to_string(state_path)
        .map(|t| parse_state(&t))
        .unwrap_or_default();
    let hold_until = f
        .get("hold-file")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| t.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let budget = fetch::Budget {
        max_per_day: f.get_parsed("max-per-day", 500)?,
        max_per_hour: f.get_parsed("max-per-hour", 50)?,
        max_per_run: f.get_parsed("max-per-run", 50)?,
    };
    let todo = fetch::select(&slices, &state, now(), &budget, hold_until);
    let dry = f.has("dry-run");
    let mut shell = Shell {
        project: PathBuf::from(f.require("project-dir")?),
        dry,
    };
    let mut created = 0;
    for s in todo {
        fetch::submit(s, &JobParams::for_pages(s.pages.len()), &mut shell)?;
        created += 1;
        if !dry {
            let entry = StateEntry {
                created_at: now(),
                wu: s.name.clone(),
            };
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(state_path)
                .map_err(|e| format!("{state_path}: {e}"))?;
            writeln!(
                file,
                "{}",
                serde_json::to_string(&entry).map_err(|e| e.to_string())?
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(created)
}

fn pass(f: &Flags) -> Result<usize, String> {
    let manifest_path = f.require("manifest")?;
    let pages_per_wu: usize = f.get_parsed("pages-per-wu", 4)?;
    if pages_per_wu == 0 {
        return Err("--pages-per-wu must be positive".into());
    }
    let text =
        std::fs::read_to_string(manifest_path).map_err(|e| format!("{manifest_path}: {e}"))?;
    let pages = manifest::parse(&text).map_err(|e| e.to_string())?;
    let slices = manifest::slice(&pages, pages_per_wu, f.has("allow-partial"));

    let state_path = f.require("state")?;
    let state = std::fs::read_to_string(state_path)
        .map(|t| parse_state(&t))
        .unwrap_or_default();
    let todo = select(
        &slices,
        &state,
        now(),
        f.get_parsed("max-per-run", 50)?,
        f.get_parsed("max-per-day", 500)?,
    );

    let dry = f.has("dry-run");
    let mut shell = Shell {
        project: PathBuf::from(f.require("project-dir")?),
        dry,
    };
    let mut created = 0;
    for s in todo {
        submit(s, &JobParams::for_pages(s.pages.len()), &mut shell)?;
        created += 1;
        if !dry {
            let entry = StateEntry {
                created_at: now(),
                wu: s.name.clone(),
            };
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(state_path)
                .map_err(|e| format!("{state_path}: {e}"))?;
            writeln!(
                file,
                "{}",
                serde_json::to_string(&entry).map_err(|e| e.to_string())?
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(created)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let f = match Flags::parse(&args, &["allow-partial", "dry-run"]) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("usnm-boinc-workgen: {e}");
            return ExitCode::from(2);
        }
    };
    let loop_secs: u64 = match f.get_parsed("loop-secs", 0) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("usnm-boinc-workgen: {e}");
            return ExitCode::from(2);
        }
    };
    loop {
        let r = match f.get("mode") {
            None | Some("staged") => pass(&f),
            Some("fetch-and-ocr") => pass_fetch(&f),
            Some(other) => {
                eprintln!("usnm-boinc-workgen: --mode {other:?} is not staged or fetch-and-ocr");
                return ExitCode::from(2);
            }
        };
        match r {
            Ok(n) => eprintln!("usnm-boinc-workgen: created {n} workunits"),
            Err(e) => {
                eprintln!("usnm-boinc-workgen: {e}");
                if loop_secs == 0 {
                    return ExitCode::FAILURE;
                }
            }
        }
        if loop_secs == 0 {
            return ExitCode::SUCCESS;
        }
        std::thread::sleep(std::time::Duration::from_secs(loop_secs));
    }
}
