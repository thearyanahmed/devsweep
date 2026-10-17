# devsweep

What your projects' build output costs in disk space, whether it's safe to remove, and cleaning it with each tool's own
command.

```sh
make install                  # go install: puts devsweep in ~/go/bin
devsweep                      # table of everything under ~/dev
devsweep -tree                # the same as a tree
devsweep -caches              # plus shared caches: cargo, go, npm, pnpm, brew, docker, ...
devsweep -older 30d -kind rust,node ~/dev ~/work
devsweep -json                # for scripts
devsweep clean -dry-run       # what clean would run
devsweep clean                # runs the safe ones after one yes
```

## Safety

| | Meaning | Clean |
| --- | --- | --- |
| safe | the project's tooling recreates it exactly: lockfile present, gitignored | asked once for all |
| caution | likely recreatable but may differ or hold something you want: `dist/`, no lockfile, not gitignored | only with `-include-caution`, asked one by one |
| unsafe | git tracks files in it | never |

## How it cleans

Each artifact is cleaned by its own tool, run in the project: `cargo clean`, `mix clean`, `swift package reset`,
`npm run clean` (when package.json has one), `cmake --build … --target clean`. Shared caches use theirs: `go clean -cache`,
`npm cache clean`, `pnpm store prune`, `brew cleanup`, `docker system prune -f`, and so on.

Some things have no tool command (`node_modules`, `.next`, `__pycache__`, `.venv`, the cargo registry). They show as
`rm -rf` and are left alone unless you pass `-rm`.

A project is recognised by a directory name plus a marker beside it (`target` + `Cargo.toml`, `node_modules` +
`package.json`, ...): see `rules.go` to add one. Sizes are disk usage; hard links shared with something outside the
artifact are shown separately as "shared", since deleting doesn't free them.
