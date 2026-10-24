//! End to end on the synthetic corpus: turn the fixture pages back into two
//! LoC-style batch archives, run enqueue → curate → release twice (a base,
//! then a delta), and check the pipeline reproduces the checked-in fixtures
//! exactly: both indexes, the baselines, titles, places and pages per title.
//! Then load the result the way the API does.

mod american_stories;
mod cluster;
mod common;
mod congress;
mod curate;
mod dedup;
mod finish;
mod latin;
mod layout;
mod overlay;
mod release;
mod versions;
mod writer;
