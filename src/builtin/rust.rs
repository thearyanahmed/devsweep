use crate::sweep::{Registry, SimpleSweeper};

pub(super) fn register(r: &mut Registry) {
    r.add(
        SimpleSweeper::new("rust")
            .folders(&["target"])
            .markers(&["Cargo.toml"])
            .markers_inside(&["CACHEDIR.TAG"])
            .clean(&["cargo", "clean"])
            .rebuild("cargo build"),
    );
}
