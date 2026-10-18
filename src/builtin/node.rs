use std::fs;
use std::path::Path;

use crate::sweep::{Claim, Clean, Registry, Safety, SimpleSweeper, Sweeper, exists};

pub(super) fn register(r: &mut Registry) {
    r.add(Modules {
        dirs: vec!["node_modules".into()],
    });
    let pkg = &["package.json"];
    r.add(
        SimpleSweeper::new("node")
            .folders(&[".next"])
            .markers(pkg)
            .rebuild("next build / next dev"),
    );
    r.add(
        SimpleSweeper::new("node")
            .folders(&[".nuxt"])
            .markers(pkg)
            .clean(&["npx", "nuxi", "cleanup"])
            .rebuild("nuxt build / nuxt dev"),
    );
    r.add(
        SimpleSweeper::new("node")
            .folders(&[".svelte-kit"])
            .markers(pkg)
            .rebuild("svelte-kit sync"),
    );
    r.add(
        SimpleSweeper::new("node")
            .folders(&[".turbo"])
            .markers(&["package.json", "turbo.json"])
            .rebuild("turbo (cache)"),
    );
    r.add(
        SimpleSweeper::new("node")
            .folders(&[".parcel-cache"])
            .markers(pkg)
            .rebuild("parcel (cache)"),
    );
    r.add(
        SimpleSweeper::new("node")
            .folders(&[".angular"])
            .markers(&["angular.json"])
            .clean(&["npx", "ng", "cache", "clean"])
            .rebuild("ng build (cache)"),
    );
    r.add(
        SimpleSweeper::new("node")
            .folders(&[".vite"])
            .markers(pkg)
            .rebuild("vite (cache)"),
    );
    r.add(BuildOutput {
        dirs: vec![
            "dist".into(),
            "build".into(),
            "out".into(),
            "release".into(),
        ],
    });
}

/// `node_modules`: safe when a lockfile pins what a reinstall brings back.
#[derive(Debug)]
struct Modules {
    dirs: Vec<String>,
}

impl Sweeper for Modules {
    fn name(&self) -> &'static str {
        "node"
    }

    fn dirs(&self) -> &[String] {
        &self.dirs
    }

    fn claim(&self, _dir: &Path, project: &Path) -> Option<Claim> {
        if !exists(&project.join("package.json")) {
            return None;
        }
        Some(lock(project).map_or_else(
            || Claim {
                safety: Safety::Caution,
                why: "no lockfile: a reinstall may pick newer versions".into(),
                rebuild: "npm install".into(),
                clean: Clean::RemoveDir,
            },
            |l| Claim {
                safety: Safety::Safe,
                why: String::new(),
                rebuild: l.install.into(),
                clean: Clean::RemoveDir,
            },
        ))
    }
}

/// `dist/`, `build/`, `out/`, `release/`: a build's output, which may be a release someone wants to keep.
#[derive(Debug)]
struct BuildOutput {
    dirs: Vec<String>,
}

impl Sweeper for BuildOutput {
    fn name(&self) -> &'static str {
        "node"
    }

    fn dirs(&self) -> &[String] {
        &self.dirs
    }

    fn claim(&self, dir: &Path, project: &Path) -> Option<Claim> {
        let markers: &[&str] = if dir.file_name().is_some_and(|n| n == "release") {
            &[
                "electron-builder.yml",
                "electron-builder.json",
                "package.json",
            ]
        } else {
            &["package.json"]
        };
        if !markers.iter().any(|m| exists(&project.join(m))) {
            return None;
        }
        Some(Claim {
            safety: Safety::Caution,
            why: "build output: may be a release you want to keep".into(),
            rebuild: "the project's build script".into(),
            clean: clean_script(project).map_or(Clean::RemoveDir, Clean::Tool),
        })
    }
}

/// A Node lockfile and its package manager.
#[derive(Debug)]
struct Lock {
    pm: &'static str,
    install: &'static str,
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
fn lock(project: &Path) -> Option<&'static Lock> {
    for dir in project.ancestors() {
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
fn clean_script(project: &Path) -> Option<Vec<String>> {
    let data = fs::read(project.join("package.json")).ok()?;
    let pkg: serde_json::Value = serde_json::from_slice(&data).ok()?;
    let script = pkg.get("scripts")?.get("clean")?.as_str()?;
    if script.is_empty() {
        return None;
    }
    let pm = lock(project).map_or("npm", |l| l.pm);
    Some(vec![pm.into(), "run".into(), "clean".into()])
}
