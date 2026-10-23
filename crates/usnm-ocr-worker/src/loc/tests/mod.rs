//! The tests of [`super`], in four groups, with the stand-ins they share.

use super::*;
use crate::standin::StandIn;

mod batch;
mod challenge;
mod item_errors;
mod pacer_and_route;

pub(super) const BATCH: &str = "/storage-services/service/ndnp/xx/batch_xx_test_ver01/";

/// A client of `s` whose storage requests are `gap` apart.
pub(super) fn batch_client(
    s: &StandIn,
    gap: Duration,
    cache: Option<PathBuf>,
    own: bool,
) -> LocClient {
    LocClient::new(LocConfig {
        api_gap: Duration::from_millis(10),
        tile_gap: Duration::from_millis(10),
        storage_gap: gap,
        storage_own_budget: own,
        attempts: 3,
        backoff: Duration::from_millis(10),
        api_root: s.root.clone(),
        iiif_root: s.iiif_root(),
        storage_root: s.storage_root(),
        batches_root: s.batches_root(),
        batch_cache: cache,
        item_probes: true,
        html_is_block: false,
    })
    .unwrap()
}

pub(super) fn key(seq: u16) -> PageKey {
    format!("sn1/1890-05-03/ed-1/seq-{seq}").parse().unwrap()
}
