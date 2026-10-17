use std::fs;
use std::path::Path;

use serde::Serialize;

/// What deleting an artifact costs you.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Safety {
    /// Regenerated exactly by the project's own tooling.
    Safe,
    /// Probably regenerable, but the result may differ or it may hold something by hand.
    Caution,
    /// Tracked in git: deleting it loses or changes real content.
    Unsafe,
}

impl Safety {
    pub fn as_str(self) -> &'static str {
        match self {
            Safety::Safe => "safe",
            Safety::Caution => "caution",
            Safety::Unsafe => "unsafe",
        }
    }
}

/// How a project recreates an artifact.
#[derive(Clone, Copy)]
pub enum Rebuild {
    Fixed(&'static str),
    NodeInstall,
    PythonEnv,
    BuildOutput,
}

/// The tool's own command for removing an artifact, run in the project dir. `None` means no tool offers one, so the
/// only way is rm -rf, which devsweep does only when asked with -rm.
#[derive(Clone, Copy)]
pub enum Clean {
    None,
    Tool(&'static [&'static str]),
    Script,
    Cmake,
}

/// Recognises one kind of build output by its directory name and a marker beside it.
pub struct Rule {
    pub dir: &'static str,
    pub kind: &'static str,
    /// Any one of these in the parent dir (or "@name" inside the artifact) identifies the project.
    markers: &'static [&'static str],
    pub safety: Safety,
    pub rebuild: Rebuild,
    pub clean: Clean,
}

const fn rule(
    dir: &'static str,
    kind: &'static str,
    markers: &'static [&'static str],
    rebuild: Rebuild,
) -> Rule {
    Rule {
        dir,
        kind,
        markers,
        safety: Safety::Safe,
        rebuild,
        clean: Clean::None,
    }
}

impl Rule {
    const fn clean(mut self, c: Clean) -> Self {
        self.clean = c;
        self
    }

    const fn caution(mut self) -> Self {
        self.safety = Safety::Caution;
        self
    }
}

use Rebuild::{BuildOutput, Fixed, NodeInstall, PythonEnv};

const PKG: &[&str] = &["package.json"];
const CMAKE: &[&str] = &["CMakeLists.txt"];

static RULES: &[Rule] = &[
    // Rust
    rule(
        "target",
        "rust",
        &["Cargo.toml", "@CACHEDIR.TAG"],
        Fixed("cargo build"),
    )
    .clean(Clean::Tool(&["cargo", "clean"])),
    // Node: no package manager has a command that removes node_modules or a framework's cache, so these are rm only.
    rule("node_modules", "node", PKG, NodeInstall),
    rule(".next", "node", PKG, Fixed("next build / next dev")),
    rule(".nuxt", "node", PKG, Fixed("nuxt build / nuxt dev"))
        .clean(Clean::Tool(&["npx", "nuxi", "cleanup"])),
    rule(".svelte-kit", "node", PKG, Fixed("svelte-kit sync")),
    rule(
        ".turbo",
        "node",
        &["package.json", "turbo.json"],
        Fixed("turbo (cache)"),
    ),
    rule(".parcel-cache", "node", PKG, Fixed("parcel (cache)")),
    rule(
        ".angular",
        "node",
        &["angular.json"],
        Fixed("ng build (cache)"),
    )
    .clean(Clean::Tool(&["npx", "ng", "cache", "clean"])),
    rule(".vite", "node", PKG, Fixed("vite (cache)")),
    rule("dist", "node", PKG, BuildOutput)
        .caution()
        .clean(Clean::Script),
    rule("build", "node", PKG, BuildOutput)
        .caution()
        .clean(Clean::Script),
    rule("out", "node", PKG, BuildOutput)
        .caution()
        .clean(Clean::Script),
    rule(
        "release",
        "node",
        &[
            "electron-builder.yml",
            "electron-builder.json",
            "package.json",
        ],
        BuildOutput,
    )
    .caution()
    .clean(Clean::Script),
    // Go
    rule("vendor", "go", &["go.mod"], Fixed("go mod vendor")).caution(),
    // Python
    rule(".venv", "python", &["@pyvenv.cfg"], PythonEnv),
    rule("venv", "python", &["@pyvenv.cfg"], PythonEnv),
    rule(
        ".tox",
        "python",
        &["tox.ini", "pyproject.toml", "setup.cfg"],
        Fixed("tox"),
    ),
    rule(".nox", "python", &["noxfile.py"], Fixed("nox")),
    rule(".pytest_cache", "python", &[], Fixed("(cache)")),
    rule(".mypy_cache", "python", &[], Fixed("(cache)")),
    rule(".ruff_cache", "python", &[], Fixed("(cache)")).clean(Clean::Tool(&["ruff", "clean"])),
    rule("__pycache__", "python", &[], Fixed("(cache)")),
    // Swift, Zig, Elixir, CMake
    rule(".build", "swift", &["Package.swift"], Fixed("swift build"))
        .clean(Clean::Tool(&["swift", "package", "reset"])),
    rule(".zig-cache", "zig", &["build.zig"], Fixed("zig build")),
    rule("zig-cache", "zig", &["build.zig"], Fixed("zig build")),
    rule("zig-out", "zig", &["build.zig"], Fixed("zig build")),
    rule("_build", "elixir", &["mix.exs"], Fixed("mix compile"))
        .clean(Clean::Tool(&["mix", "clean", "--deps"])),
    rule("deps", "elixir", &["mix.exs"], Fixed("mix deps.get")).clean(Clean::Tool(&[
        "mix",
        "deps.clean",
        "--all",
    ])),
    rule(
        "build",
        "cmake",
        &["@CMakeCache.txt"],
        Fixed("cmake --build"),
    )
    .clean(Clean::Cmake),
    rule("cmake-build-debug", "cmake", CMAKE, Fixed("cmake --build")).clean(Clean::Cmake),
    rule(
        "cmake-build-release",
        "cmake",
        CMAKE,
        Fixed("cmake --build"),
    )
    .clean(Clean::Cmake),
];

/// The rule that claims the directory `name` at `path`, whose parent is `parent`, if any.
pub fn find(path: &Path, parent: &Path, name: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| {
        r.dir == name
            && (r.markers.is_empty()
                || r.markers.iter().any(|m| match m.strip_prefix('@') {
                    Some(inside) => exists(&path.join(inside)),
                    None => exists(&parent.join(m)),
                }))
    })
}

impl Rebuild {
    /// The command that recreates the artifact, how safe that is, and why when it isn't.
    pub fn resolve(self, parent: &Path) -> (String, Safety, String) {
        match self {
            Fixed(cmd) => (cmd.into(), Safety::Safe, String::new()),
            NodeInstall => match node_lock(parent) {
                Some(l) => (l.install.into(), Safety::Safe, String::new()),
                None => (
                    "npm install".into(),
                    Safety::Caution,
                    "no lockfile: a reinstall may pick newer versions".into(),
                ),
            },
            PythonEnv => {
                const LOCKS: &[(&str, &str)] = &[
                    ("uv.lock", "uv sync"),
                    ("poetry.lock", "poetry install"),
                    ("Pipfile.lock", "pipenv install"),
                    ("requirements.txt", "pip install -r requirements.txt"),
                ];
                match LOCKS.iter().find(|(f, _)| exists(&parent.join(f))) {
                    Some((_, cmd)) => ((*cmd).into(), Safety::Safe, String::new()),
                    None => (
                        "recreate by hand".into(),
                        Safety::Caution,
                        "no lockfile or requirements.txt: packages were installed by hand".into(),
                    ),
                }
            }
            BuildOutput => (
                "the project's build script".into(),
                Safety::Caution,
                "build output: may be a release you want to keep".into(),
            ),
        }
    }
}

impl Clean {
    pub fn resolve(self, parent: &Path, dir: &str) -> Option<Vec<String>> {
        match self {
            Clean::None => None,
            Clean::Tool(argv) => Some(argv.iter().map(|s| s.to_string()).collect()),
            Clean::Script => clean_script(parent),
            Clean::Cmake => Some(
                ["cmake", "--build", dir, "--target", "clean"]
                    .map(String::from)
                    .to_vec(),
            ),
        }
    }
}

/// A Node lockfile and its package manager.
pub struct Lock {
    pub pm: &'static str,
    pub install: &'static str,
}

const LOCKFILES: &[(&str, Lock)] = &[
    (
        "pnpm-lock.yaml",
        Lock {
            pm: "pnpm",
            install: "pnpm install",
        },
    ),
    (
        "package-lock.json",
        Lock {
            pm: "npm",
            install: "npm ci",
        },
    ),
    (
        "yarn.lock",
        Lock {
            pm: "yarn",
            install: "yarn install",
        },
    ),
    (
        "bun.lock",
        Lock {
            pm: "bun",
            install: "bun install",
        },
    ),
    (
        "bun.lockb",
        Lock {
            pm: "bun",
            install: "bun install",
        },
    ),
];

/// The lockfile for a Node project, looking upwards since a workspace keeps one at its root.
fn node_lock(parent: &Path) -> Option<&'static Lock> {
    for dir in parent.ancestors() {
        if let Some((_, l)) = LOCKFILES.iter().find(|(f, _)| exists(&dir.join(f))) {
            return Some(l);
        }
        if exists(&dir.join(".git")) {
            break;
        }
    }
    None
}

/// The project's own "clean" script, when package.json has one.
fn clean_script(parent: &Path) -> Option<Vec<String>> {
    let data = fs::read(parent.join("package.json")).ok()?;
    let pkg: serde_json::Value = serde_json::from_slice(&data).ok()?;
    let script = pkg.get("scripts")?.get("clean")?.as_str()?;
    if script.is_empty() {
        return None;
    }
    let pm = node_lock(parent).map_or("npm", |l| l.pm);
    Some(vec![pm.into(), "run".into(), "clean".into()])
}

pub fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}
