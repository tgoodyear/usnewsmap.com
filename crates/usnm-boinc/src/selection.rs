//! Explicit, ordered source membership for bounded shipment operations.
use anyhow::{ensure, Context};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub(crate) fn members(path: &Path) -> anyhow::Result<Vec<String>> {
    let docs: Vec<String> = serde_json::from_slice(&crate::read(path, 64 * 1024)?)?;
    ensure!(
        !docs.is_empty() && docs.len() <= 500,
        "membership must contain 1 to 500 sources"
    );
    ensure!(
        docs.iter().collect::<BTreeSet<_>>().len() == docs.len(),
        "duplicate membership"
    );
    for doc in &docs {
        crate::assess::check_doc_id(doc)?;
    }
    Ok(docs)
}

pub(crate) fn units(
    dir: &Path,
    file: &str,
    selection: Option<&Path>,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()?;
    paths.retain(|path| path.join(file).exists());
    paths.sort();
    let Some(selection) = selection else {
        return Ok(paths);
    };
    let docs = members(selection)?;
    let mut ordered = Vec::new();
    for path in paths {
        let job: serde_json::Value = serde_json::from_slice(&crate::read(
            &path.join(file),
            usnm_ocr_contract::quality::MAX_JOB_JSON.max(usnm_ocr_contract::qwen::MAX_JOB_JSON),
        )?)?;
        let members: Vec<&str> = match job["pages"].as_array().filter(|pages| !pages.is_empty()) {
            Some(pages) => pages
                .iter()
                .map(|p| p["doc_id"].as_str().context("missing doc_id"))
                .collect::<Result<_, _>>()?,
            None => vec![job["doc_id"].as_str().context("missing doc_id")?],
        };
        let indexes: Vec<_> = members
            .iter()
            .filter_map(|doc| docs.iter().position(|d| d == doc))
            .collect();
        if let Some(first) = indexes.iter().min() {
            ensure!(
                indexes.len() == members.len() || path.join("submitted").is_file(),
                "pending unit crosses shipment membership: {}",
                path.display()
            );
            ordered.push((*first, path));
        }
    }
    ordered.sort_by_key(|(index, _)| *index);
    Ok(ordered.into_iter().map(|(_, path)| path).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_is_ordered_and_rejects_cross_boundary_pending_units() {
        let dir = tempfile::tempdir().unwrap();
        let a = "sn1_1900-01-01_ed-1_seq-1";
        let b = "sn1_1900-01-01_ed-1_seq-2";
        let c = "sn1_1900-01-01_ed-1_seq-3";
        for doc in [a, b, c] {
            std::fs::create_dir(dir.path().join(doc)).unwrap();
            crate::atomic(
                &dir.path().join(doc).join("qwen.json"),
                &serde_json::to_vec(&serde_json::json!({"doc_id": doc})).unwrap(),
            )
            .unwrap();
        }
        let selected = dir.path().join("members.json");
        crate::atomic(&selected, &serde_json::to_vec(&[b, a]).unwrap()).unwrap();
        assert_eq!(
            units(dir.path(), "qwen.json", Some(&selected)).unwrap(),
            vec![dir.path().join(b), dir.path().join(a)]
        );
        crate::atomic(
            &dir.path().join(b).join("qwen.json"),
            &serde_json::to_vec(&serde_json::json!({"pages": [{"doc_id": b}, {"doc_id": c}]}))
                .unwrap(),
        )
        .unwrap();
        assert!(units(dir.path(), "qwen.json", Some(&selected))
            .unwrap_err()
            .to_string()
            .contains("crosses"));
        crate::atomic(&dir.path().join(b).join("submitted"), b"receipt").unwrap();
        assert_eq!(
            units(dir.path(), "qwen.json", Some(&selected))
                .unwrap()
                .len(),
            2
        );
    }
}
