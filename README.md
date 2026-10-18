# devsweep

What your projects' build output costs in disk space, whether it's safe to remove, and cleaning it with each tool's own
command.

```sh
make install                  # cargo install: puts devsweep in ~/.cargo/bin
devsweep                      # table of everything under ~/dev
devsweep -tree                # the same as a tree
devsweep -caches              # plus shared caches: cargo, go, npm, pnpm, brew, docker, ...
devsweep -older 30d -kind rust,node ~/dev ~/work
devsweep -sort size -reverse  # smallest first; -sort age or path, -reverse works for each
devsweep -json                # for scripts
devsweep clean --dry-run      # what clean would run
devsweep clean                # runs the safe ones after one yes
```

## Safety

| | Meaning | Clean |
| --- | --- | --- |
| safe | the project's tooling recreates it exactly: lockfile present, gitignored | asked once for all |
| caution | likely recreatable but may differ or hold something you want: `dist/`, no lockfile, not gitignored | only with `--include-caution`, asked one by one |
| unsafe | git tracks files in it | never |

## How it cleans

Each artifact is cleaned by its own tool, run in the project: `cargo clean`, `mix clean`, `swift package reset`,
`npm run clean` (when package.json has one), `cmake --build … --target clean`. Shared caches use theirs: `go clean -cache`,
`npm cache clean`, `pnpm store prune`, `brew cleanup`, `docker system prune -f`, and so on.

Some things have no tool command (`node_modules`, `.next`, `__pycache__`, `.venv`, the cargo registry). They show as
`rm -rf` and are left alone unless you pass `-rm`, and even then `rm -rf` only shows what it would delete until you
also pass `--dry-run=false`. Tool commands like `cargo clean` run without it.

Docker is listed too, after the projects: its images and its volumes, each with the containers using it (`-images`
or `-volumes` shows only those). An image's size is the layers only it has; layers shared with other images are shown
apart, since removing it doesn't free them. Anything a running container uses is unsafe. A dangling image (`<none>`,
an old build) is safe; any other image is caution, since a re-pull or rebuild may differ, and so is every volume, since
volumes hold data. Docker only removes an image or volume no container uses, so `devsweep clean -images` and
`clean -volumes` offer just those, one by one, and name the containers in the way of the rest. It never removes a
container. `-group image` shows Docker by image instead: each image, its containers with their state and size, and
the volumes they use.

`-group project`, `-group kind` or `-group safety` groups the listing, under a heading per group with its size.

A project is recognised by a directory name plus a marker beside it (`target` + `Cargo.toml`, `node_modules` +
`package.json`, ...): see `src/rules.rs` to add one. Sizes are disk usage; hard links shared with something outside the
artifact are shown separately as "shared", since deleting doesn't free them.
