use crate::sweep::{Registry, SimpleSweeper};

pub(super) fn register(r: &mut Registry) {
    r.add(
        SimpleSweeper::new("swift")
            .folders(&[".build"])
            .markers(&["Package.swift"])
            .clean(&["swift", "package", "reset"])
            .rebuild("swift build"),
    );
}
