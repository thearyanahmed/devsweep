use crate::sweep::{Registry, SimpleSweeper};

pub(super) fn register(r: &mut Registry) {
    r.add(
        SimpleSweeper::new("zig")
            .folders(&[".zig-cache", "zig-cache", "zig-out"])
            .markers(&["build.zig"])
            .rebuild("zig build"),
    );
}
