# Changelog

All notable changes to devsweep are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- More shared caches with `-caches`: Gradle, Maven, Hugging Face, poetry, pre-commit, deno, node-gyp, Playwright,
  Cypress and Puppeteer browsers, Composer, Dart pub, Terraform plugins, ccache, sccache, CocoaPods, Carthage,
  SwiftPM, Xcode archives, device support, documentation cache and device logs, simulator caches and logs, the
  Android SDK cache, and JetBrains, VS Code and Cursor cached data. Ones that are slow or impossible to refill exactly
  are caution.

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

[Unreleased]: https://github.com/thearyanahmed/devsweep/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/thearyanahmed/devsweep/releases/tag/v0.1.0
