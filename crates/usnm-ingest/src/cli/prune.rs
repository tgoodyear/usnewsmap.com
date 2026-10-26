//! `prune`: deleting the Quickwit indexes no published version names.
use anyhow::{bail, Context};
use clap::Args;
use usnm_ingest::release::{self, Published};
use usnm_ingest::sink::QuickwitNode;
use usnm_ingest::state::State;
use usnm_state::owner_id;

use super::common::{root, IndexTarget, Stores};
use crate::Cli;

/// Delete the Quickwit indexes that neither the serving version nor its
/// rollback names (#245): older versions' and stopped rebuilds'. Without
/// `--delete`, only lists them with their sizes (a dry run). Takes the
/// writer lock like a release, and refuses if `current.json` can't be
/// read (docs/operations/rebuild-and-index-storage.md, "Prune old index versions").
#[derive(Args)]
pub(crate) struct PruneArgs {
    /// Delete the listed indexes. Without it nothing is deleted.
    #[arg(long)]
    delete: bool,
    /// The Quickwit node on the index store: `--quickwit-bin` (with its
    /// metastore and index root), or for a dry run `--quickwit-url`.
    #[command(flatten)]
    target: IndexTarget,
}

/// `prune`, holding the writer lock throughout (released on every path),
/// against `--quickwit-url` or a node run from `--quickwit-bin` for its
/// length. The node's data goes in a temporary directory: it only reads
/// the metastore and deletes, and a job execution has no scratch share.
async fn prune(
    cli: &Stores,
    state: &State,
    delete: bool,
    t: &IndexTarget,
) -> anyhow::Result<usnm_ingest::prune::Report> {
    prune_target(delete, t)?;
    let reference = usnm_store::open(&cli.reference)?;
    // Before the lock and the node: refuse at once without a published version.
    usnm_ingest::prune::kept(reference.as_ref(), state).await?;
    let owner = owner_id();
    let lease = release::lock_writer(state, &owner).await?;
    let result = async {
        match (&t.quickwit_url, &t.quickwit_bin) {
            (Some(url), None) => {
                usnm_ingest::prune::prune(state, reference.as_ref(), url, delete, &lease).await
            }
            (None, Some(bin)) => {
                let metastore = t
                    .quickwit_metastore
                    .as_deref()
                    .context("--quickwit-bin needs --quickwit-metastore")?;
                let dir = tempfile::Builder::new().prefix("usnm-prune").tempdir()?;
                let node =
                    QuickwitNode::start(bin, dir.path(), t.quickwit_port, metastore, root(t)?)
                        .await?;
                let r =
                    usnm_ingest::prune::prune(state, reference.as_ref(), &node.url, delete, &lease)
                        .await;
                node.stop().await?;
                r
            }
            _ => bail!("choose one of --quickwit-url or --quickwit-bin"),
        }
    }
    .await;
    let unlocked = release::unlock_writer(state, &owner, lease).await;
    let report = result?;
    unlocked?;
    Ok(report)
}

/// Which nodes prune may use. Deleting needs a node it started
/// (`--quickwit-bin`): when the writer lock can't be renewed it stops that
/// node, which ends a delete under way. A node at `--quickwit-url` isn't
/// its to stop, and Quickwit carries on with a delete whose caller went
/// away, so that one is for dry runs.
fn prune_target(delete: bool, t: &IndexTarget) -> anyhow::Result<()> {
    if t.index_dir.is_some() {
        bail!("prune needs a Quickwit node: --quickwit-bin, or --quickwit-url for a dry run");
    }
    if delete && t.quickwit_url.is_some() && t.quickwit_bin.is_none() {
        bail!(
            "prune --delete needs --quickwit-bin: it stops its own node if the writer lock \
             is lost, and can't stop a node at --quickwit-url"
        );
    }
    Ok(())
}

pub(crate) async fn prune_command(
    cli: &Cli,
    state: State,
    args: &PruneArgs,
) -> anyhow::Result<Option<Published>> {
    let PruneArgs { delete, target } = args;
    let report = prune(&cli.stores, &state, *delete, target).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Command;
    use clap::Parser;

    #[test]
    fn prune_is_a_dry_run_unless_asked_to_delete() {
        let parse = |extra: &[&str]| {
            let mut args = vec![
                "usnm-ingest",
                "--curated",
                "c",
                "--reference",
                "r",
                "prune",
                "--quickwit-url",
                "http://127.0.0.1:7280",
            ];
            args.extend_from_slice(extra);
            match Cli::try_parse_from(args).unwrap().command {
                Command::Prune(PruneArgs { delete, target }) => (delete, target.quickwit_url),
                _ => panic!("not a prune"),
            }
        };
        assert_eq!(parse(&[]), (false, Some("http://127.0.0.1:7280".into())));
        assert!(parse(&["--delete"]).0);
        // Named flags only (#277).
        assert!(Cli::try_parse_from([
            "usnm-ingest",
            "--curated",
            "c",
            "--reference",
            "r",
            "prune",
            "delete"
        ])
        .is_err());
    }

    #[test]
    fn prune_deletes_only_through_a_node_it_started() {
        let target = |args: &[&str]| {
            let mut all = vec!["usnm-ingest", "--curated", "c", "--reference", "r", "prune"];
            all.extend_from_slice(args);
            match Cli::try_parse_from(all).unwrap().command {
                Command::Prune(PruneArgs { target, .. }) => target,
                _ => panic!("not a prune"),
            }
        };
        let url = target(&["--quickwit-url", "http://127.0.0.1:7280"]);
        assert!(prune_target(false, &url).is_ok());
        let err = prune_target(true, &url).unwrap_err();
        assert!(
            format!("{err:#}").contains("needs --quickwit-bin"),
            "{err:#}"
        );
        let bin = target(&["--quickwit-bin", "/usr/local/bin/quickwit"]);
        assert!(prune_target(true, &bin).is_ok());
        assert!(prune_target(false, &target(&["--index-dir", "x"])).is_err());
    }
}
