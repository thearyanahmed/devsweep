//! devsweep finds the build output projects leave behind (target/, `node_modules`/, .venv/, ...), Docker's images and
//! volumes and the shared tool caches, says how big each one is and whether removing it is safe, and cleans it on
//! request.

//!
//! The library is what the `devsweep` command runs: [`cli::run`] with a [`sweep::Registry`]. To teach it something
//! new, implement [`sweep::Sweeper`] (or build a [`sweep::SimpleSweeper`]), add it to the registry, and run that.

mod builtin;
mod clean;
pub mod cli;
mod cmd;
mod output;
mod scan;
pub mod sweep;

pub use scan::{Artifact, scan};
