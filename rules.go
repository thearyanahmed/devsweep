package main

import (
	"encoding/json"
	"os"
	"path/filepath"
)

// Safety says what deleting an artifact costs you.
type Safety int

const (
	Safe    Safety = iota // regenerated exactly by the project's own tooling
	Caution               // probably regenerable, but the result may differ or it may hold something by hand
	Unsafe                // tracked in git: deleting it loses or changes real content
)

func (s Safety) String() string {
	return [...]string{"safe", "caution", "unsafe"}[s]
}

// A rule recognises one kind of build output by its directory name and a marker beside it.
type rule struct {
	dir     string
	kind    string
	markers []string // any one of these in the parent dir (or "@name" inside the artifact) identifies the project
	safety  Safety
	rebuild func(parent string) (cmd string, safety Safety, why string)
	// clean is the tool's own command for removing it, run in the project dir. Nil means no tool offers one, so
	// the only way is rm -rf, which devsweep does only when asked with -rm.
	clean func(parent, dir string) []string
}

func fixed(cmd string) func(string) (string, Safety, string) {
	return func(string) (string, Safety, string) { return cmd, Safe, "" }
}

func tool(argv ...string) func(string, string) []string {
	return func(string, string) []string { return argv }
}

var rules = []rule{
	// Rust
	{dir: "target", kind: "rust", markers: []string{"Cargo.toml", "@CACHEDIR.TAG"}, rebuild: fixed("cargo build"), clean: tool("cargo", "clean")},

	// Node: no package manager has a command that removes node_modules or a framework's cache, so these are rm only.
	{dir: "node_modules", kind: "node", markers: []string{"package.json"}, rebuild: nodeInstall},
	{dir: ".next", kind: "node", markers: []string{"package.json"}, rebuild: fixed("next build / next dev")},
	{dir: ".nuxt", kind: "node", markers: []string{"package.json"}, rebuild: fixed("nuxt build / nuxt dev"), clean: tool("npx", "nuxi", "cleanup")},
	{dir: ".svelte-kit", kind: "node", markers: []string{"package.json"}, rebuild: fixed("svelte-kit sync")},
	{dir: ".turbo", kind: "node", markers: []string{"package.json", "turbo.json"}, rebuild: fixed("turbo (cache)")},
	{dir: ".parcel-cache", kind: "node", markers: []string{"package.json"}, rebuild: fixed("parcel (cache)")},
	{dir: ".angular", kind: "node", markers: []string{"angular.json"}, rebuild: fixed("ng build (cache)"), clean: tool("npx", "ng", "cache", "clean")},
	{dir: ".vite", kind: "node", markers: []string{"package.json"}, rebuild: fixed("vite (cache)")},
	{dir: "dist", kind: "node", markers: []string{"package.json"}, safety: Caution, rebuild: buildOutput, clean: cleanScript},
	{dir: "build", kind: "node", markers: []string{"package.json"}, safety: Caution, rebuild: buildOutput, clean: cleanScript},
	{dir: "out", kind: "node", markers: []string{"package.json"}, safety: Caution, rebuild: buildOutput, clean: cleanScript},
	{dir: "release", kind: "node", markers: []string{"electron-builder.yml", "electron-builder.json", "package.json"}, safety: Caution, rebuild: buildOutput, clean: cleanScript},

	// Go
	{dir: "vendor", kind: "go", markers: []string{"go.mod"}, safety: Caution, rebuild: fixed("go mod vendor")},

	// Python
	{dir: ".venv", kind: "python", markers: []string{"@pyvenv.cfg"}, rebuild: pythonEnv},
	{dir: "venv", kind: "python", markers: []string{"@pyvenv.cfg"}, rebuild: pythonEnv},
	{dir: ".tox", kind: "python", markers: []string{"tox.ini", "pyproject.toml", "setup.cfg"}, rebuild: fixed("tox")},
	{dir: ".nox", kind: "python", markers: []string{"noxfile.py"}, rebuild: fixed("nox")},
	{dir: ".pytest_cache", kind: "python", rebuild: fixed("(cache)")},
	{dir: ".mypy_cache", kind: "python", rebuild: fixed("(cache)")},
	{dir: ".ruff_cache", kind: "python", rebuild: fixed("(cache)"), clean: tool("ruff", "clean")},
	{dir: "__pycache__", kind: "python", rebuild: fixed("(cache)")},

	// Swift, Zig, Elixir, CMake
	{dir: ".build", kind: "swift", markers: []string{"Package.swift"}, rebuild: fixed("swift build"), clean: tool("swift", "package", "reset")},
	{dir: ".zig-cache", kind: "zig", markers: []string{"build.zig"}, rebuild: fixed("zig build")},
	{dir: "zig-cache", kind: "zig", markers: []string{"build.zig"}, rebuild: fixed("zig build")},
	{dir: "zig-out", kind: "zig", markers: []string{"build.zig"}, rebuild: fixed("zig build")},
	{dir: "_build", kind: "elixir", markers: []string{"mix.exs"}, rebuild: fixed("mix compile"), clean: tool("mix", "clean", "--deps")},
	{dir: "deps", kind: "elixir", markers: []string{"mix.exs"}, rebuild: fixed("mix deps.get"), clean: tool("mix", "deps.clean", "--all")},
	{dir: "build", kind: "cmake", markers: []string{"@CMakeCache.txt"}, rebuild: fixed("cmake --build"), clean: cmakeClean},
	{dir: "cmake-build-debug", kind: "cmake", markers: []string{"CMakeLists.txt"}, rebuild: fixed("cmake --build"), clean: cmakeClean},
	{dir: "cmake-build-release", kind: "cmake", markers: []string{"CMakeLists.txt"}, rebuild: fixed("cmake --build"), clean: cmakeClean},
}

// match returns the rule that claims dir (a directory at path whose parent is parent), if any.
func match(path, parent, name string) *rule {
	for i := range rules {
		r := &rules[i]
		if r.dir != name {
			continue
		}
		if len(r.markers) == 0 {
			return r
		}
		for _, m := range r.markers {
			p := filepath.Join(parent, m)
			if m[0] == '@' {
				p = filepath.Join(path, m[1:])
			}
			if exists(p) {
				return r
			}
		}
	}
	return nil
}

// lockfiles maps a Node lockfile to its package manager, in the order they're looked for.
var lockfiles = []struct{ file, pm, install string }{
	{"pnpm-lock.yaml", "pnpm", "pnpm install"},
	{"package-lock.json", "npm", "npm ci"},
	{"yarn.lock", "yarn", "yarn install"},
	{"bun.lock", "bun", "bun install"},
	{"bun.lockb", "bun", "bun install"},
}

// nodeLock finds the lockfile for a Node project, looking upwards since a workspace keeps one at its root.
func nodeLock(parent string) (pm, install string, ok bool) {
	for dir := parent; ; dir = filepath.Dir(dir) {
		for _, l := range lockfiles {
			if exists(filepath.Join(dir, l.file)) {
				return l.pm, l.install, true
			}
		}
		if exists(filepath.Join(dir, ".git")) || filepath.Dir(dir) == dir {
			return "npm", "npm install", false
		}
	}
}

func nodeInstall(parent string) (string, Safety, string) {
	if _, install, ok := nodeLock(parent); ok {
		return install, Safe, ""
	}
	return "npm install", Caution, "no lockfile: a reinstall may pick newer versions"
}

// cleanScript uses the project's own "clean" script, when package.json has one.
func cleanScript(parent, _ string) []string {
	data, err := os.ReadFile(filepath.Join(parent, "package.json"))
	if err != nil {
		return nil
	}
	var pkg struct{ Scripts map[string]string }
	if json.Unmarshal(data, &pkg) != nil || pkg.Scripts["clean"] == "" {
		return nil
	}
	pm, _, _ := nodeLock(parent)
	return []string{pm, "run", "clean"}
}

func cmakeClean(_, dir string) []string {
	return []string{"cmake", "--build", dir, "--target", "clean"}
}

func pythonEnv(parent string) (string, Safety, string) {
	locks := []struct{ file, cmd string }{
		{"uv.lock", "uv sync"},
		{"poetry.lock", "poetry install"},
		{"Pipfile.lock", "pipenv install"},
		{"requirements.txt", "pip install -r requirements.txt"},
	}
	for _, l := range locks {
		if exists(filepath.Join(parent, l.file)) {
			return l.cmd, Safe, ""
		}
	}
	return "recreate by hand", Caution, "no lockfile or requirements.txt: packages were installed by hand"
}

func buildOutput(string) (string, Safety, string) {
	return "the project's build script", Caution, "build output: may be a release you want to keep"
}

func exists(p string) bool {
	_, err := os.Lstat(p)
	return err == nil
}
