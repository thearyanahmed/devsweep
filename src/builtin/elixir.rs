use crate::sweep::{Registry, SimpleSweeper};

pub(super) fn register(r: &mut Registry) {
    r.add(
        SimpleSweeper::new("elixir")
            .folders(&["_build"])
            .markers(&["mix.exs"])
            .clean(&["mix", "clean", "--deps"])
            .rebuild("mix compile"),
    );
    r.add(
        SimpleSweeper::new("elixir")
            .folders(&["deps"])
            .markers(&["mix.exs"])
            .clean(&["mix", "deps.clean", "--all"])
            .rebuild("mix deps.get"),
    );
}
