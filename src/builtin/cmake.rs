use crate::sweep::{Registry, SimpleSweeper};

pub(super) fn register(r: &mut Registry) {
    let clean = ["cmake", "--build", "{dir}", "--target", "clean"];
    // A build folder is CMake's when it holds a CMakeCache.txt.
    r.add(
        SimpleSweeper::new("cmake")
            .folders(&["build"])
            .markers_inside(&["CMakeCache.txt"])
            .clean(&clean)
            .rebuild("cmake --build"),
    );
    r.add(
        SimpleSweeper::new("cmake")
            .folders(&[
                "cmake-build-debug",
                "cmake-build-release",
                "cmake-build-relwithdebinfo",
                "cmake-build-minsizerel",
            ])
            .markers(&["CMakeLists.txt"])
            .clean(&clean)
            .rebuild("cmake --build"),
    );
}
