# Contributing to devsweep

Thanks for helping. Bug reports, new sweepers and fixes are all welcome.

## Reporting a bug

Open an issue with:

- what you ran, and what you expected;
- what devsweep printed (`-json` output is the most useful, with private paths trimmed);
- your OS, `devsweep` version (`cargo install --list | grep devsweep`) and, for Docker problems, which engine
  (Docker Desktop, OrbStack, Colima).

If devsweep removed something it shouldn't have, or offered to, say so in the title: those come first.

## Making a change

For anything bigger than a small fix, open an issue first so we can agree on the approach.

```sh
git clone https://github.com/thearyanahmed/devsweep
cd devsweep
make test    # clippy with every lint as an error, then the tests
```

`make test` is what CI runs, on macOS and Linux; a pull request needs it green.

The minimum supported Rust is 1.88 (`rust-version` in `Cargo.toml`), and CI also builds and tests on it. Raising it
is a deliberate change: bump `rust-version` and the CI job together, and note it in the changelog.

### The lints

They are strict on purpose and live in `Cargo.toml` and `clippy.toml`: clippy's `pedantic`, `nursery` and `cargo`
groups and a set of `restriction` lints are errors, as are dead code, anything unused and missing docs on public items.

- `#[allow]` isn't permitted. When a lint is wrong for a line, use `#[expect(lint, reason = "why")]`.
- No `.unwrap()` outside tests; `.expect("why this can't fail")` instead.
- Format with `cargo fmt`.

### Style

Match the code around you. Comments and docs are short plain sentences that say why, not what.

### Tests

Add a test with the change. Unit tests sit in a `tests` module beside the code; tests of the public API, such as a
custom sweeper, go in `tests/`.

## Adding a sweeper

A new kind of project goes in `src/builtin/`, one file per ecosystem, registered in `src/builtin/mod.rs`. Most are a
few lines of `SimpleSweeper`; see `src/builtin/rust.rs`. Implement `Sweeper` yourself only when the safety or the
commands depend on what's in the project, as `node.rs` and `python.rs` do.

Before proposing one:

- the folder must be recognised by a marker (a file beside it or inside it), never by its name alone;
- prefer the tool's own clean command over `rm -rf`;
- rate it `safe` only when the project's tooling recreates it exactly; otherwise `caution`, with a reason;
- add a row to the "What it finds" table in [docs/reference.md](docs/reference.md).

Order in `register` matters only when two sweepers claim the same folder name: the first to claim it keeps it.

## Commits and pull requests

- One logical change per commit, with a one-line message in the imperative: "Add Gradle sweeper", not "Added gradle".
- Note user-visible changes under `[Unreleased]` in `CHANGELOG.md`.
- Update [docs/reference.md](docs/reference.md) when behavior or flags change, and the README if the change belongs
  in the short version.

## Releasing

1. Bump `version` in `Cargo.toml`, and in `CHANGELOG.md` turn `[Unreleased]` into the new version with today's date.
2. Commit, tag (`git tag v0.3.0`) and push the tag.

The release workflow builds macOS and Linux binaries, publishes a GitHub release with that version's changelog notes,
and updates `Formula/devsweep.rb` in [thearyanahmed/homebrew-tap](https://github.com/thearyanahmed/homebrew-tap). The
tap step needs a `HOMEBREW_TAP_TOKEN` secret: a token that can push to that repository. Without it, the step is
skipped and the release still goes out.

## License

By contributing, you agree that your contributions are dual licensed under MIT OR Apache-2.0, as described in the
README, without any additional terms or conditions.
