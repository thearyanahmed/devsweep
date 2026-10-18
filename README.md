# devsweep

Build output and Docker quietly eat a disk: a `target/` per Rust project, a `node_modules` per Node one, images and
volumes nobody remembers. devsweep shows what is taking the space, whether each piece is safe to remove, and cleans it
with each tool's own command, asking first.

```
~/dev  7 artifacts in 5 projects, 3.1 GiB safe  5.8 MiB caution  0 B unsafe
4 artifacts (520.0 KiB) hidden by -min 1M; newest built 3h ago
SIZE       SAFETY   KIND  PROJECT                          DIR           LAST BUILT  CLEAN WITH           REBUILD WITH
1.1 GiB    safe     rust  paperkites/kites/backend         target        3h ago      cargo clean          cargo build
909.1 MiB  safe     rust  paperkites/kites-purchase        target        3h ago      cargo clean          cargo build
882.7 MiB  safe     node  paperkites/kites                 node_modules  3h ago      rm -rf node_modules  pnpm install
5.8 MiB    caution  node  thearyanahmed/thearyanahmed.com  dist          1m ago      rm -rf dist          the project's build script
```

## Install

Needs Rust 1.88 or newer. macOS or Linux; Docker is optional.

```sh
cargo install --git https://github.com/thearyanahmed/devsweep
```

From a checkout:

```sh
make install    # cargo install --path .: a release build in ~/.cargo/bin
make build      # or just build ./devsweep
```

## Usage

```sh
devsweep                          # everything under ~/dev, then Docker's images and volumes
devsweep ~/dev ~/work             # other folders, any number
devsweep -tree                    # the same as a tree of folders
devsweep -group kind              # grouped: project, kind, safety, or image (Docker)
devsweep -sort age:asc            # most recently built first; size, age, path, :asc or :desc
devsweep -older 30d -kind rust    # only Rust projects not built for a month
devsweep -caches                  # also the shared caches: cargo, go, npm, pnpm, brew, ...
devsweep -images -volumes         # only Docker
devsweep -json                    # everything, for scripts

devsweep clean --dry-run          # what clean would run, running nothing
devsweep clean                    # runs the safe tool commands after one yes
devsweep clean -caches -images    # also offers caches and Docker images, one by one
```

Flags go before or after the folders. One-word flags take one dash (`-tree`), several words two (`--dry-run`); a
one-word flag also takes two. `-h` lists them all.

## What it finds

A project is recognised by a folder name plus a marker beside it, so a `target/` without a `Cargo.toml` is left alone.

| Kind | Folders | Cleaned with | Recreated with |
| --- | --- | --- | --- |
| rust | `target` | `cargo clean` | `cargo build` |
| node | `node_modules` | rm -rf | the lockfile's install: `pnpm install`, `npm ci`, `yarn install`, `bun install` |
| node | `.next`, `.svelte-kit`, `.turbo`, `.parcel-cache`, `.vite` | rm -rf | the framework's build or dev server |
| node | `.nuxt`, `.angular` | `npx nuxi cleanup`, `npx ng cache clean` | the framework's build |
| node | `dist`, `build`, `out`, `release` | the project's `clean` script, else rm -rf | the project's build script |
| go | `vendor` | rm -rf | `go mod vendor` |
| python | `.venv`, `venv` | rm -rf | `uv sync`, `poetry install`, `pipenv install` or `pip install -r requirements.txt` |
| python | `.tox`, `.nox`, `.pytest_cache`, `.mypy_cache`, `__pycache__` | rm -rf | running the tool again |
| python | `.ruff_cache` | `ruff clean` | running ruff again |
| swift | `.build` | `swift package reset` | `swift build` |
| zig | `.zig-cache`, `zig-cache`, `zig-out` | rm -rf | `zig build` |
| elixir | `_build`, `deps` | `mix clean --deps`, `mix deps.clean --all` | `mix compile`, `mix deps.get` |
| cmake | `build` (with a `CMakeCache.txt`), `cmake-build-debug`, `-release`, `-relwithdebinfo`, `-minsizerel` | `cmake --build … --target clean` | `cmake --build` |

Each kind is a sweeper in `src/builtin/`, one file per ecosystem; to add your own, see [Adding your own](#adding-your-own).

## Safety

| | Meaning | `clean` |
| --- | --- | --- |
| safe | the project's own tooling recreates it exactly: a lockfile is there and git ignores it | asked once for all of them |
| caution | likely recreatable, but the result may differ or it may hold something you want: `dist/`, no lockfile, not gitignored | only with `--include-caution`, asked one by one |
| unsafe | git tracks files in it | never |

Git decides the last word: a folder with tracked files is unsafe whatever it's called, and one git doesn't ignore is at
least caution.

## Cleaning

`devsweep clean` takes the same folders and filters as the listing, shows what it will do, and asks.

**Tool commands first.** Each artifact is cleaned by its own tool, run in its project: `cargo clean`, `mix clean`,
`swift package reset`, `npm run clean`. Several artifacts sharing one command (a Cargo workspace) run it once.

**rm -rf only when asked, and dry by default.** Some things have no tool that removes them (`node_modules`, `.next`,
`__pycache__`, `.venv`). They show as `rm -rf` and are left alone unless you pass `-rm`, and even then `rm -rf` only
shows what it would delete:

| You pass | Tool commands | rm -rf |
| --- | --- | --- |
| nothing | run, after the usual yes | only shown |
| `--dry-run` | only shown | only shown |
| `--dry-run=false` | run | deletes |

Every `rm -rf` waits for a step of its own at the end, after all the tools' commands: the folders are listed together
with their sizes and asked about once. At the end devsweep reports what was freed, measured again afterwards, and
what was only shown. A command that fails doesn't count as freed.

Some things it refuses outright, whatever a sweeper says:

- running `clean` as root or under `sudo`;
- `/` or your home folder as a folder to scan;
- an `rm -rf` of anything but a real folder (not a symlink) strictly inside a scanned folder or your home folder,
  never one of those itself, checked again right before deleting.

## Docker

Docker is part of every listing, after the projects, read from one `docker system df --verbose`:

- **Images**: the size only that image takes, with the layers it shares with other images shown apart
  (`+2.9 MiB shared`), since removing it doesn't free those.
- **Volumes**: their size.
- For both, the containers using them and whether each is running.

| | Images | Volumes |
| --- | --- | --- |
| a running container uses it | unsafe | unsafe |
| only stopped containers use it | caution | caution |
| nothing uses it | caution (a re-pull or rebuild may differ); safe if dangling (`<none>`, an old build) | caution (it holds data) |

Docker only removes an image or a volume no container uses, even a stopped one. So `clean -images` and
`clean -volumes` offer just those, one by one, through `docker image rm` and `docker volume rm`, and for the rest name
the containers in the way (`docker rm api first`). devsweep never removes a container.

`-group image` shows Docker by image instead: each image, the containers made from it with their state and their own
size (files they changed, and their logs), and the volumes those use. `-images` or `-volumes` on their own show only
Docker, without scanning folders.

devsweep goes through the `docker` command, so it asks whichever engine the current context points at: Docker Desktop,
OrbStack, Colima. With Docker missing or stopped, the Docker part is simply left out; a Docker that doesn't answer
within 5 seconds is skipped.

## Shared caches

`-caches` adds the tool-wide caches outside any project, refilled on demand; only the ones present are listed, and on
Linux the tools' `~/.cache` and `~/.config` folders stand in for `~/Library`. `clean -caches` offers each one, through
the tool's own command where it has one:

| Cache | Cleaned with |
| --- | --- |
| cargo registry, registry sources, git checkouts | rm -rf (cargo also trims these itself) |
| go build cache, module cache | `go clean -cache`, `go clean -modcache` |
| npm, pnpm store, yarn, bun, corepack | `npm cache clean --force`, `pnpm store prune`, `yarn cache clean`, `bun pm cache rm`, `corepack cache clean` |
| deno, node-gyp headers | `deno clean`, rm -rf |
| Playwright, Cypress, Puppeteer browsers | rm -rf |
| pip, uv, poetry, pre-commit | `pip3 cache purge`, `uv cache prune`, rm -rf, `pre-commit clean` |
| Hugging Face models and datasets (caution) | rm -rf |
| Gradle caches and wrapper distributions | rm -rf (run `gradle --stop` first) |
| Maven repository (caution: holds what `mvn install` put there) | rm -rf |
| Composer, Dart pub (caution: holds global packages), Terraform plugins | `composer clear-cache`, `dart pub cache clean --force`, rm -rf |
| ccache, sccache | `ccache --clear`, rm -rf |
| Homebrew | `brew cleanup --prune=all` |
| CocoaPods, Carthage, SwiftPM | `pod cache clean --all`, rm -rf |
| Xcode derived data, documentation cache, device logs, iOS and watchOS device support | rm -rf |
| Xcode archives (caution: the dSYMs for your shipped builds' crash reports) | rm -rf |
| Simulator caches and logs, unavailable iOS simulators | rm -rf, `xcrun simctl delete unavailable` |
| Android SDK cache | rm -rf |
| JetBrains IDEs, VS Code and Cursor cached data, VS Code extension downloads | rm -rf (quit the editor first) |
| Electron, electron-builder downloads | rm -rf |
| Docker's reclaimable space | `docker system prune -f` (caution) |

## Listing

- **`-group project|kind|safety|image`**: a heading per group with its count and total, the rows under it, and the
  column the heading already says left out. Groups follow `-sort`; safety groups always go safe, caution, unsafe.
  `-tree` with `-group kind` or `safety` draws a tree per group (the tree already groups by project).
- **`-sort size|age|path`**: biggest, longest unbuilt or A to Z first. Add `:asc` or `:desc` for the other way round
  (`size:asc` is smallest first, `path:desc` Z to A). Rows inside groups and the groups themselves both follow it.
- **`-min`** (default 1M), **`-older`**, **`-kind`** filter; a line under the summary says how many artifacts they
  hid and why, so an empty result isn't a mystery.
- **`-json`**: artifacts, roots, Docker's images, containers and volumes, and with `-caches` the caches.

Output is colored on a terminal and plain otherwise, or with `NO_COLOR` set.

## Flags

| Flag | |
| --- | --- |
| `-caches` | also list shared tool caches; with clean, offer each one |
| `--dry-run` | clean: show what would run, run nothing; rm -rf is always a dry run unless `--dry-run=false` |
| `--include-caution` | clean: also offer caution artifacts, each asked one by one |
| `-group` | group by `project`, `kind` or `safety`; `image` arranges Docker by image |
| `-images` | only Docker images; with clean, offer each one |
| `-json` | print JSON |
| `-kind` | only these kinds, comma separated: `rust,node,go,python,swift,zig,elixir,cmake`, or any sweeper you add |
| `-min` | hide artifacts smaller than this, e.g. `100M`, `1G` (default `1M`) |
| `-older` | only artifacts not built for this long, e.g. `14d`, `3w`, `2mo`, `12h` |
| `-rm` | clean: also offer what only rm -rf removes (`node_modules`, `.next`, ...) |
| `-sort` | `size` (default), `age` or `path`, optionally with `:asc` or `:desc` |
| `-tree` | a tree instead of a table |
| `-volumes` | only Docker volumes; with clean, offer each one |

## Adding your own

devsweep is a library with a thin binary on top. Everything it knows goes through one trait, `Sweeper`: the built-in
Rust, Node and Docker support are sweepers like any other. To teach it something new, write a small binary of your own:

```rust
use devsweep::sweep::{Registry, SimpleSweeper};

fn main() {
    let mut registry = Registry::builtin();
    registry.add(
        SimpleSweeper::new("gradle")
            .folders(&["build", ".gradle"])
            .markers(&["build.gradle", "build.gradle.kts"])
            .clean(&["./gradlew", "clean"])
            .rebuild("./gradlew build"),
    );
    devsweep::cli::run(&registry);
}
```

`SimpleSweeper` covers "folders with these names, beside one of these files, cleaned and rebuilt by these commands".
For anything smarter, implement `Sweeper` yourself:

- `dirs` and `claim` for folders met while walking: say whether a folder is yours, how safe removing it is, how it's
  cleaned (`Clean::Tool`, `Clean::RemoveDir` or `Clean::Blocked`) and rebuilt.
- `list` and `sections` for things found on their own, outside any project (a cache, Docker's images): return
  `Item`s, and describe the table they're listed in.

The engine does the rest: walking, measuring, rendering, asking before cleaning, and the safety net, which a sweeper
can't switch off. Git-tracked files make a folder unsafe whatever the sweeper says, a folder git doesn't ignore is at
least caution, and `rm -rf` stays a dry run unless `--dry-run=false`. The first registered sweeper to claim a folder
keeps it, so the built-ins can't be overridden by accident. `tests/custom_sweeper.rs` shows each of these through the
public API.

## How it works

- **Sizes** are disk usage (allocated blocks), not file lengths. A hard-linked file counts once, and counts as
  "shared" when some of its links live outside the artifact, since deleting the artifact doesn't free it.
- **Speed**: every artifact's folders go onto one queue that all cores work through, so a single huge `node_modules`
  is spread across them. The git checks, the cache sizes and the Docker query run at the same time as the scan.
  `~/dev` with 57,000 files takes about 80 ms.
- **Git**: `git ls-files` and `git check-ignore` in the project decide whether an artifact is tracked or ignored.

## Development

```sh
make test    # clippy with every lint as an error, then the tests
```

The lints are strict and live in `Cargo.toml` and `clippy.toml`: clippy's `pedantic`, `nursery` and `cargo` groups and a
set of `restriction` lints are all errors, as are dead code and anything unused. `#[allow]` isn't permitted; an
exception is an `#[expect(…, reason = "…")]` that says why.

[CONTRIBUTING.md](CONTRIBUTING.md) covers the rest: how to propose a change, add a sweeper, and what CI checks.
Changes are recorded in [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as
defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
