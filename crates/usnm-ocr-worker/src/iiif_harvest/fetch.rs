//! `--action fetch`: the images in a saved plan, one at a time, within `robots.txt` and the
//! pace. Files land in `<out>/<issue id>/<canvas index>.<ext>`, each with a sidecar
//! `<file>.json`. A file that matches its sidecar's sha256 is never fetched again; one that
//! doesn't is kept as it is, never fetched again or written over, and reported.

use std::path::Path;

use anyhow::Context;
use tokio::sync::watch;

use super::plan::{image_path, terms_notice, Plan};
use super::{json_of, sidecar_path, template, Action, Client, Options, Sidecar, Summary};

/// This many failures in a row end the run.
const MAX_IN_A_ROW: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Have {
    No,
    /// The file's sha256 matches its sidecar's.
    Verified,
    /// A file is there but no sidecar vouches for it.
    Unverified,
}

impl Have {
    pub fn is_verified(&self) -> bool {
        *self == Self::Verified
    }
}

fn sha_of_file(path: &Path) -> std::io::Result<String> {
    use sha2::Digest;
    let mut f = std::fs::File::open(path)?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = std::io::Read::read(&mut f, &mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(usnm_core::digest::hex(&h.finalize()))
}

/// Whether `dest` is already in hand as the image at `url` of the manifest `manifest_id`:
/// its sidecar names that URL and manifest and its sha256 matches the bytes. A file for
/// another URL (a replan with a different size, say) is `Unverified`: kept, not replaced.
pub fn have(dest: &Path, url: &str, manifest_id: &str) -> Have {
    if !dest.exists() {
        return Have::No;
    }
    let recorded = std::fs::read(sidecar_path(dest))
        .ok()
        .and_then(|b| serde_json::from_slice::<Sidecar>(&b).ok());
    match (recorded, sha_of_file(dest)) {
        (Some(s), Ok(sha))
            if s.sha256 == sha
                && s.url == url
                && s.citation
                    .as_ref()
                    .is_some_and(|c| c.manifest_id == manifest_id) =>
        {
            Have::Verified
        }
        _ => Have::Unverified,
    }
}

/// [`have`] for a planned canvas.
pub fn have_for(out: &Path, i: &super::plan::PlanIssue, c: &super::plan::PlanCanvas) -> Have {
    have(&image_path(out, &i.id, c), &c.url, &i.citation.manifest_id)
}

fn problem(out: &Path, what: &str, message: &str) {
    let line = serde_json::json!({"at": super::now(), "what": what, "message": message});
    let r = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out.join("problems.jsonl"))
        .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes()));
    if let Err(e) = r {
        tracing::warn!(error = %e, "can't record a problem");
    }
}

fn cell(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// `CITATIONS.tsv`: issue label, persistent link or manifest id, attribution, accessed date,
/// for each issue with an image on disk. An earlier file is kept beside it.
fn citations(out: &Path, plan: &Plan) -> anyhow::Result<()> {
    let mut text = String::from("issue_label\tlink\tattribution\taccessed\n");
    for i in &plan.issues {
        let accessed = i
            .canvases
            .iter()
            .filter_map(|c| {
                let b = std::fs::read(sidecar_path(&image_path(out, &i.id, c))).ok()?;
                let s = serde_json::from_slice::<Sidecar>(&b).ok()?;
                // Only bytes this plan asked for: the same URL and manifest.
                (s.url == c.url
                    && s.citation
                        .is_some_and(|m| m.manifest_id == i.citation.manifest_id))
                .then_some(s.accessed)
            })
            .max();
        let Some(accessed) = accessed else { continue };
        let label = match i.citation.label_text() {
            l if l.is_empty() => i.id.clone(),
            l => l,
        };
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            cell(&label),
            cell(i.citation.link_or_id()),
            cell(&i.citation.attribution_text()),
            accessed.split('T').next().unwrap_or(&accessed)
        ));
    }
    let path = out.join("CITATIONS.tsv");
    if path.exists() {
        crate::fetcher::set_aside(&path)?;
    }
    crate::ghn::write_atomic(&path, text.as_bytes())?;
    Ok(())
}

pub async fn run(opts: Options, stop: watch::Receiver<bool>) -> anyhow::Result<Summary> {
    debug_assert!(opts.action == Action::Fetch);
    let plan_path = opts
        .plan
        .clone()
        .unwrap_or_else(|| opts.out.join("plan.json"));
    let bytes = std::fs::read(&plan_path).with_context(|| plan_path.display().to_string())?;
    let plan: Plan = serde_json::from_value(json_of(&bytes, "the plan")?)?;
    eprintln!("{}", terms_notice(plan.terms_url.as_deref()));

    let mut hosts = super::hosts_of(&plan.collection_url, &opts.allow_hosts)?;
    for i in &plan.issues {
        for c in &i.canvases {
            let u = reqwest::Url::parse(&c.url).with_context(|| c.url.clone())?;
            let h = u.host_str().unwrap_or_default().to_owned();
            if !hosts.contains(&h) {
                anyhow::bail!(
                    "the plan has images on {h}, which this run may not ask; after reading its \
                     robots.txt and terms, add --allow-host {h}"
                );
            }
        }
    }
    let mut summary = Summary {
        action: "fetch".to_owned(),
        issues: plan.issues_count,
        images_planned: plan.images_count,
        ..Summary::default()
    };
    hosts.sort();
    let client = Client::new(&opts.out, &opts.state_dir, &hosts, opts.pace, stop)?;
    *client.terms_url.lock().expect("terms") = plan.terms_url.clone();
    let first = plan
        .issues
        .iter()
        .flat_map(|i| &i.canvases)
        .map(|c| c.url.clone())
        .next();
    if let Some(u) = &first {
        client.check_not_slowed(u).await?;
    }

    // Every path is checked against robots.txt before the first image is asked for.
    for i in &plan.issues {
        for c in &i.canvases {
            if !have_for(&opts.out, i, c).is_verified() {
                client.check_robots(&c.url).await?;
            }
        }
    }

    let mut fetched_now = 0usize;
    let mut in_a_row = 0usize;
    'all: for i in &plan.issues {
        for c in &i.canvases {
            let dest = image_path(&opts.out, &i.id, c);
            match have_for(&opts.out, i, c) {
                Have::Verified => {
                    summary.already_have += 1;
                    continue;
                }
                Have::Unverified => {
                    summary.kept_unverified += 1;
                    problem(
                        &opts.out,
                        &dest.display().to_string(),
                        "a file is on disk without a matching sidecar; kept, not fetched again",
                    );
                    continue;
                }
                Have::No => {}
            }
            if opts.limit.is_some_and(|l| fetched_now >= l) {
                break 'all;
            }
            std::fs::create_dir_all(dest.parent().context("no parent")?)?;
            let mut t = template("image", plan.terms_url.as_deref());
            t.issue = Some(i.id.clone());
            t.canvas_index = Some(c.index);
            t.citation = Some(i.citation.clone());
            match client.get(&c.url, &dest, opts.max_image_bytes, t).await {
                Ok(_) => {
                    fetched_now += 1;
                    summary.fetched += 1;
                    in_a_row = 0;
                }
                Err(f) => {
                    let ends = super::ends_run(&f);
                    let msg = format!("{}/{:04}: {:#}", i.id, c.index, super::why(f));
                    problem(&opts.out, &c.url, &msg);
                    summary.failed.push(msg.clone());
                    in_a_row += 1;
                    if ends {
                        summary.stopped = Some(msg);
                        break 'all;
                    }
                    if in_a_row >= MAX_IN_A_ROW {
                        summary.stopped =
                            Some(format!("{in_a_row} failures in a row; last: {msg}"));
                        break 'all;
                    }
                }
            }
        }
    }
    // What is still to do, at the pace.
    let left = plan
        .issues
        .iter()
        .flat_map(|i| i.canvases.iter().map(move |c| (i, c)))
        .filter(|(i, c)| !have_for(&opts.out, i, c).is_verified())
        .count();
    summary.estimated_requests = left;
    summary.estimated_seconds =
        (left as f64 * plan.gap_secs.max(opts.pace.gap().as_secs_f64())).ceil() as u64;
    citations(&opts.out, &plan)?;
    crate::ghn::write_atomic(
        &opts.out.join("status.json"),
        &serde_json::to_vec_pretty(&summary)?,
    )?;
    Ok(summary)
}
