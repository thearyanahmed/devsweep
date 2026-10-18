//! The `devsweep` binary: everything lives in the library.
#![expect(
    unused_crate_dependencies,
    reason = "the library uses them; the binary only calls it"
)]

fn main() {
    devsweep::cli::run(&devsweep::sweep::Registry::builtin());
}
