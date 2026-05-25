//! Internal library re-exports so integration tests can drive the same
//! harness code the `animus-plugin-harness` binary uses.

pub mod protocol;
pub mod scenarios;
pub mod spawn;

pub use protocol::run_all;
