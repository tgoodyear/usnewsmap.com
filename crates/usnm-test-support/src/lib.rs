//! Helpers shared by the workspace's tests. Only ever a dev-dependency.

mod script;

pub use script::{write_executable, write_executable_with_mode};
