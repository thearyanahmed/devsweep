# Changelog

All notable changes to devsweep are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-10-18

### Added

- `devsweep why`: where the disk went, on one screen. The disk's size and free space, what devsweep can reclaim by
  part with the command for each, what Docker's VM disk holds back and how each engine gives it back, and Time
  Machine's local snapshots with the `tmutil` command that thins them.
- `clean -until 30G`: clean only until the disk has that much free, safest and longest-unbuilt artifacts first.
- `clean -trim 30d`: Rust projects lose only the build files unused that long, through cargo-sweep, instead of all of
  `target/`.
- `clean -y` answers yes to every question but an unsafe one's. What is offered still follows the other flags, and
  `rm -rf` stays a dry run unless `--dry-run=false`.
- `-pwd` scans, or with `clean` cleans, the folder you are in.
- Old toolchains with `-toolchains`: rustup, nvm, fnm, volta, pyenv, mise and SDKMAN versions, except the default,
  the one on PATH and any a project pins (`.nvmrc`, `rust-toolchain.toml`, `.tool-versions`, ...).
- Local AI models: each Ollama model (removed with `ollama rm`) and LM Studio's models folder; `-models` lists only
  them, and with `clean` offers each one.
- Local Kubernetes clusters: kind and k3d clusters listed by cluster; `clean -clusters` deletes each with its own tool.
- `-git` lists `.git` folders over 500 MiB and worktrees whose repository is gone; never cleaned.
- About 30 more shared caches with `-caches`: Gradle, Maven, Hugging Face, poetry, pre-commit, deno, node-gyp,
  Playwright, Cypress and Puppeteer browsers, Composer, Dart pub, Terraform plugins, ccache, sccache, CocoaPods,
  Carthage, SwiftPM, minikube, the Android SDK cache, and JetBrains, VS Code and Cursor cached data.
- Xcode: simulator runtimes (the newest per platform is never offered), archives, documentation cache, device logs,
  simulator caches and logs, device support split per OS version, and old Xcode copies in `/Applications`, listed
  with how to remove them.
- Prebuilt binaries for macOS and Linux on every release, and a Homebrew tap.

### Changed

- With no folders named, devsweep scans your home folder instead of `~/dev`, skipping `~/Library`, hidden folders at
  its top, the media folders and Photos libraries; `clean` accepts the home folder too. Named folders are checked
  first: a missing, unreadable or macOS-protected one stops devsweep with the reason.
- Naming folders (or `-pwd`) lists only what's in them: Docker and local models are left out unless their flag asks.
- `--include-caution` is replaced by `-upto safe|caution|unsafe`. `-upto unsafe` can clean folders git tracks files
  in: one by one, each confirmed by typing its name (never by `-y`), and only with no uncommitted changes in it.
- `-tree` shows only each artifact's size, safety and clean command; `-detailed` brings back its kind, last build,
  rebuild command and why it isn't safe.
- `Sweeper::list` is given the folders being scanned.
- The README is short; everything else is in `docs/reference.md`.

## [0.1.0]

The first release.

### Added

- Listing of build output under any folders (`~/dev` by default) as a table, a tree (`-tree`) or JSON (`-json`), with
  each artifact's size, safety, last build, and the commands that clean and rebuild it.
- Built-in support for Rust, Node, Go, Python, Swift, Zig, Elixir and CMake projects.
- Safety ratings (safe, caution, unsafe), with git deciding the last word: tracked files make a folder unsafe, and one
  git doesn't ignore is at least caution.
- Docker images, volumes and the containers using them, from one `docker system df --verbose`; `-group image` arranges
  them by image.
- Shared tool caches with `-caches`: cargo, go, npm, pnpm, yarn, bun, corepack, pip, uv, Homebrew, Xcode, Electron.
- `devsweep clean`, which runs each tool's own clean command after one yes, offers caution artifacts, caches, images
  and volumes one by one, and keeps `rm -rf` a dry run unless `--dry-run=false`.
- Grouping (`-group project|kind|safety|image`), sorting (`-sort size|age|path` with `:asc` or `:desc`) and filters
  (`-min`, `-older`, `-kind`).
- A library API around the `Sweeper` trait and `SimpleSweeper` builder, for adding kinds of your own.

[Unreleased]: https://github.com/thearyanahmed/devsweep/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/thearyanahmed/devsweep/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/thearyanahmed/devsweep/releases/tag/v0.1.0
