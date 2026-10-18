//! The sweepers devsweep ships with, one file per ecosystem.

mod caches;
mod cmake;
mod docker;
mod elixir;
mod go;
mod models;
mod node;
mod python;
mod rust;
mod simulators;
mod swift;
mod zig;

use crate::sweep::Registry;

/// Adds every built-in sweeper. Order matters only where two claim the same folder name: Node's `build` (beside a
/// package.json) is asked before `CMake`'s (holding a `CMakeCache.txt`).
pub(crate) fn register(r: &mut Registry) {
    rust::register(r);
    node::register(r);
    go::register(r);
    python::register(r);
    swift::register(r);
    zig::register(r);
    elixir::register(r);
    cmake::register(r);
    r.add(docker::Docker);
    r.add(models::Models);
    r.add(caches::Caches);
}
