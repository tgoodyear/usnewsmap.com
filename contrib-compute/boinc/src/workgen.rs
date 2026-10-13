//! Turn slices into BOINC `create_work` invocations.
//!
//! The generator does not talk to the BOINC database. It builds the argument lists for
//! `bin/stage_file` and `bin/create_work` (documented in the BOINC wiki, "Local job
//! submission") and runs them through a [`Submitter`], which tests replace with a recorder.

use crate::manifest::{Slice, StagedPage};
use serde::{Deserialize, Serialize};

/// Workunit parameters. Values follow docs/design.md section 5.1. The FLOPs figures are
/// a first guess and need recalibrating from the first results.
#[derive(Clone, Debug, PartialEq)]
pub struct JobParams {
    pub delay_bound_secs: u64,
    pub max_error_results: u32,
    pub max_total_results: u32,
    pub min_quorum: u32,
    pub rsc_disk_bound: u64,
    pub rsc_fpops_bound: f64,
    pub rsc_fpops_est: f64,
    pub rsc_memory_bound: u64,
    pub target_nresults: u32,
}

/// Seconds of one core for one frak2021 page, from the maintainer's survey (160 to 330 s).
pub const SECS_PER_PAGE: f64 = 250.0;
/// Assumed per-core FLOPS of a volunteer host, used only to turn seconds into an estimate.
pub const ASSUMED_FLOPS: f64 = 3.0e9;

impl JobParams {
    pub fn for_pages(n_pages: usize) -> JobParams {
        let est = n_pages as f64 * SECS_PER_PAGE * ASSUMED_FLOPS;
        JobParams {
            delay_bound_secs: 5 * 24 * 3600,
            max_error_results: 3,
            max_total_results: 6,
            min_quorum: 1,
            rsc_disk_bound: 200 * 1024 * 1024,
            rsc_fpops_bound: est * 10.0,
            rsc_fpops_est: est,
            rsc_memory_bound: 2 * 1024 * 1024 * 1024,
            target_nresults: 1,
        }
    }
}

/// The small JSON file each workunit carries as its first input, `manifest.json` on the host.
/// Fields are in alphabetical order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkunitManifest {
    pub pages: Vec<ManifestPage>,
    pub schema: u32,
    pub wu: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPage {
    /// Logical name of the image in the slot directory, `page0.jp2` and so on.
    pub file: String,
    pub language: String,
    pub md5: String,
    pub page_id: String,
    pub profile: String,
}

pub fn workunit_manifest(slice: &Slice) -> WorkunitManifest {
    WorkunitManifest {
        pages: slice
            .pages
            .iter()
            .enumerate()
            .map(|(i, p)| ManifestPage {
                file: format!("page{i}.jp2"),
                language: p.language.clone(),
                md5: p.md5.clone(),
                page_id: p.page_id.clone(),
                profile: p.profile.clone(),
            })
            .collect(),
        schema: 1,
        wu: slice.name.clone(),
    }
}

/// File name under which the manifest is staged in the download directory.
pub fn manifest_file_name(wu: &str) -> String {
    format!("{wu}_manifest.json")
}

/// Argument list for `bin/stage_file`, which copies a local file into the download hierarchy.
pub fn stage_args(local_path: &str) -> Vec<String> {
    vec!["--copy".into(), local_path.into()]
}

/// Argument list for `bin/create_work`. The manifest is input 0 and the images follow in page
/// order, matching the file_info numbering in the input template.
pub fn create_work_args(slice: &Slice, params: &JobParams) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut push = |k: &str, v: String| {
        a.push(format!("--{k}"));
        a.push(v);
    };
    push("appname", crate::APP_NAME.into());
    push("wu_name", slice.name.clone());
    push("wu_template", format!("templates/{}_in", crate::APP_NAME));
    push(
        "result_template",
        format!("templates/{}_out", crate::APP_NAME),
    );
    push("rsc_fpops_est", format!("{:.0}", params.rsc_fpops_est));
    push("rsc_fpops_bound", format!("{:.0}", params.rsc_fpops_bound));
    push("rsc_memory_bound", params.rsc_memory_bound.to_string());
    push("rsc_disk_bound", params.rsc_disk_bound.to_string());
    push("delay_bound", params.delay_bound_secs.to_string());
    push("min_quorum", params.min_quorum.to_string());
    push("target_nresults", params.target_nresults.to_string());
    push("max_error_results", params.max_error_results.to_string());
    push("max_total_results", params.max_total_results.to_string());
    a.push(manifest_file_name(&slice.name));
    for p in &slice.pages {
        push_remote(&mut a, p);
    }
    a
}

fn push_remote(a: &mut Vec<String>, p: &StagedPage) {
    a.push("--remote_file".into());
    a.push(p.url.clone());
    a.push(p.bytes.to_string());
    a.push(p.md5.clone());
}

/// Runs the BOINC tools. The real implementation shells out; tests record calls.
pub trait Submitter {
    fn write_manifest(&mut self, file_name: &str, json: &str) -> Result<String, String>;
    fn stage_file(&mut self, args: &[String]) -> Result<(), String>;
    fn create_work(&mut self, args: &[String]) -> Result<(), String>;
}

/// Names of workunits already created, from the state file (one JSON object per line).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateEntry {
    pub created_at: u64,
    pub wu: String,
}

pub fn parse_state(text: &str) -> Vec<StateEntry> {
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Pick the slices to create now: skip any whose name is already in the state, and stop at
/// `max_per_run` and at the number still allowed in the last 24 hours (`max_per_day`).
pub fn select<'a>(
    slices: &'a [Slice],
    state: &[StateEntry],
    now: u64,
    max_per_run: usize,
    max_per_day: usize,
) -> Vec<&'a Slice> {
    let recent = state.iter().filter(|e| e.created_at + 86_400 > now).count();
    let room = max_per_day.saturating_sub(recent).min(max_per_run);
    slices
        .iter()
        .filter(|s| !state.iter().any(|e| e.wu == s.name))
        .take(room)
        .collect()
}

/// Stage the manifest and create the workunit.
pub fn submit(slice: &Slice, params: &JobParams, sub: &mut dyn Submitter) -> Result<(), String> {
    let json = serde_json::to_string(&workunit_manifest(slice)).map_err(|e| e.to_string())?;
    let local = sub.write_manifest(&manifest_file_name(&slice.name), &json)?;
    sub.stage_file(&stage_args(&local))?;
    sub.create_work(&create_work_args(slice, params))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{slice, tests::page};

    fn four() -> Slice {
        let pages: Vec<_> = (0..4)
            .map(|i| page(&format!("p{i}"), "deu", "frak2021"))
            .collect();
        slice(&pages, 4, false).remove(0)
    }

    #[derive(Default)]
    struct Rec {
        calls: Vec<String>,
    }
    impl Submitter for Rec {
        fn write_manifest(&mut self, name: &str, json: &str) -> Result<String, String> {
            self.calls.push(format!("write {name} {}", json.len()));
            Ok(format!("/tmp/{name}"))
        }
        fn stage_file(&mut self, args: &[String]) -> Result<(), String> {
            self.calls.push(format!("stage {}", args.join(" ")));
            Ok(())
        }
        fn create_work(&mut self, args: &[String]) -> Result<(), String> {
            self.calls.push(format!("create {}", args.len()));
            Ok(())
        }
    }

    #[test]
    fn params_follow_the_design() {
        let p = JobParams::for_pages(4);
        assert_eq!(p.min_quorum, 1);
        assert_eq!(p.target_nresults, 1);
        assert_eq!(p.delay_bound_secs, 432_000);
        assert!((p.rsc_fpops_est - 3.0e12).abs() < 1.0);
        assert!((p.rsc_fpops_bound - 3.0e13).abs() < 1.0);
    }

    #[test]
    fn manifest_names_images_in_order_and_has_sorted_keys() {
        let s = four();
        let m = workunit_manifest(&s);
        assert_eq!(m.pages[2].file, "page2.jp2");
        assert_eq!(m.pages[2].page_id, "p2");
        let j = serde_json::to_string(&m).unwrap();
        assert!(j.find("\"pages\"").unwrap() < j.find("\"schema\"").unwrap());
        assert!(j.find("\"schema\"").unwrap() < j.find("\"wu\"").unwrap());
    }

    #[test]
    fn create_work_has_manifest_first_then_remote_files() {
        let s = four();
        let a = create_work_args(&s, &JobParams::for_pages(4));
        let pos = a
            .iter()
            .position(|x| *x == manifest_file_name(&s.name))
            .unwrap();
        assert_eq!(a[pos + 1], "--remote_file");
        assert_eq!(a[pos + 2], s.pages[0].url);
        assert_eq!(a[pos + 3], "4500000");
        assert_eq!(a[pos + 4], s.pages[0].md5);
        assert_eq!(a.iter().filter(|x| *x == "--remote_file").count(), 4);
        let i = a.iter().position(|x| x == "--min_quorum").unwrap();
        assert_eq!(a[i + 1], "1");
        assert_eq!(
            a[a.iter().position(|x| x == "--appname").unwrap() + 1],
            "usnm_ocr"
        );
    }

    #[test]
    fn submit_writes_stages_then_creates() {
        let mut r = Rec::default();
        submit(&four(), &JobParams::for_pages(4), &mut r).unwrap();
        assert!(r.calls[0].starts_with("write usnm_ocr_"));
        assert!(r.calls[1].starts_with("stage --copy /tmp/usnm_ocr_"));
        assert!(r.calls[2].starts_with("create "));
    }

    #[test]
    fn select_skips_existing_and_applies_both_caps() {
        let pages: Vec<_> = (0..20)
            .map(|i| page(&format!("p{i}"), "deu", "frak2021"))
            .collect();
        let sl = slice(&pages, 4, false); // 5 slices
        let now = 1_000_000;
        let state = vec![
            StateEntry {
                created_at: now - 10,
                wu: sl[0].name.clone(),
            },
            StateEntry {
                created_at: now - 200_000,
                wu: "old".into(),
            },
        ];
        // 1 recent entry; day cap 3 leaves room for 2; run cap 10.
        let got = select(&sl, &state, now, 10, 3);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, sl[1].name);
        // Run cap lower than the day room.
        assert_eq!(select(&sl, &state, now, 1, 3).len(), 1);
        // Day cap already used.
        assert!(select(&sl, &state, now, 10, 1).is_empty());
    }

    #[test]
    fn state_lines_parse_and_bad_lines_are_ignored() {
        let t = "{\"created_at\":5,\"wu\":\"a\"}\nnot json\n{\"created_at\":6,\"wu\":\"b\"}\n";
        let s = parse_state(t);
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].wu, "b");
    }
}
