use std::path::{Path, PathBuf};
use std::thread;

use serde::Serialize;

use crate::cmd::{on_path, output};
use crate::docker;
use crate::output::tilde;
use crate::rules::{Safety, exists};
use crate::scan::measure_all;

/// A shared, tool-wide cache outside any project.
#[derive(Debug, Serialize)]
pub struct Cache {
    pub name: &'static str,
    pub path: PathBuf,
    pub bytes: u64,
    /// The tool's own command for trimming it; `None` means only rm -rf `path`.
    #[serde(rename = "clean_cmd")]
    pub cmd: Option<Vec<String>>,
    #[serde(skip)]
    pub safety: Safety,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub note: &'static str,
    #[serde(skip)]
    docker: bool,
}

impl Cache {
    pub fn method(&self) -> String {
        self.cmd.as_ref().map_or_else(
            || format!("rm -rf {}", tilde(&self.path)),
            |argv| argv.join(" "),
        )
    }

    const fn note(mut self, note: &'static str) -> Self {
        self.note = note;
        self
    }
}

fn cache(name: &'static str, path: PathBuf, cmd: &[&str]) -> Cache {
    Cache {
        name,
        path,
        bytes: 0,
        cmd: (!cmd.is_empty()).then(|| cmd.iter().map(ToString::to_string).collect()),
        safety: Safety::Safe,
        note: "",
        docker: false,
    }
}

pub fn caches() -> Vec<Cache> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let h = |p: &str| home.join(p);
    let cargo_note = "cargo also trims it on its own";
    // Asking the tools where their caches are means starting go, pnpm (through node) and docker: do it all at once.
    let docker = on_path("docker").then(|| thread::spawn(docker::reclaimable));
    let (go, pnpm) = thread::scope(|s| {
        let pnpm = s.spawn(|| pnpm_store(h("Library/pnpm/store")));
        let go = output("go", &["env", "GOCACHE", "GOMODCACHE"]).unwrap_or_default();
        (go, pnpm.join().expect("the pnpm thread panicked"))
    });
    let mut go = go.lines().map(PathBuf::from);
    let gocache = go
        .next()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| h("Library/Caches/go-build"));
    let gomodcache = go
        .next()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| h("go/pkg/mod"));
    let list = vec![
        cache("cargo registry", h(".cargo/registry/cache"), &[]).note(cargo_note),
        cache("cargo registry src", h(".cargo/registry/src"), &[]).note(cargo_note),
        cache("cargo git", h(".cargo/git/checkouts"), &[]),
        cache("go build cache", gocache, &["go", "clean", "-cache"]),
        cache("go module cache", gomodcache, &["go", "clean", "-modcache"]),
        cache("npm cache", h(".npm"), &["npm", "cache", "clean", "--force"]),
        cache("pnpm store", pnpm, &["pnpm", "store", "prune"])
            .note("keeps packages a project still uses; prunes the store of the pnpm on PATH"),
        cache("yarn cache", h("Library/Caches/Yarn"), &["yarn", "cache", "clean"]),
        cache("bun cache", h(".bun/install/cache"), &["bun", "pm", "cache", "rm"]),
        cache("corepack", h(".cache/node/corepack"), &["corepack", "cache", "clean"]),
        cache("pip cache", h("Library/Caches/pip"), &["pip3", "cache", "purge"]),
        cache("uv cache", h(".cache/uv"), &["uv", "cache", "prune"]),
        cache("homebrew", h("Library/Caches/Homebrew"), &["brew", "cleanup", "--prune=all"]),
        cache("xcode derived data", h("Library/Developer/Xcode/DerivedData"), &[]),
        cache(
            "ios simulators (unavailable)",
            h("Library/Developer/CoreSimulator/Devices"),
            &["xcrun", "simctl", "delete", "unavailable"],
        )
        .note("only removes simulators for runtimes no longer installed"),
        cache("electron", h("Library/Caches/electron"), &[]),
        cache("electron-builder", h("Library/Caches/electron-builder"), &[]),
        Cache {
            safety: Safety::Caution,
            docker: true,
            ..cache("docker (reclaimable)", PathBuf::new(), &["docker", "system", "prune", "-f"])
                .note("stopped containers, unused networks, dangling images, build cache; add -a for all unused images")
        },
    ];

    let mut list: Vec<Cache> = list
        .into_iter()
        .filter(|c| {
            if c.docker {
                on_path("docker")
            } else {
                exists(&c.path)
            }
        })
        .collect();
    // One pass over every cache directory, while Docker answers.
    let paths: Vec<&Path> = list
        .iter()
        .filter(|c| !c.docker)
        .map(|c| c.path.as_path())
        .collect();
    let sizes = measure_all(&paths);
    for (c, m) in list.iter_mut().filter(|c| !c.docker).zip(sizes) {
        c.bytes = m.bytes;
    }
    let docker = docker.map_or(0, |d| d.join().unwrap_or(0));
    for c in list.iter_mut().filter(|c| c.docker) {
        c.bytes = docker;
    }
    // Docker not running, or nothing to reclaim: leave it out.
    list.retain(|c| c.bytes != 0);
    list
}

/// `pnpm store path` names one store version (…/store/v10); measure every version beside it.
fn pnpm_store(default: PathBuf) -> PathBuf {
    output("pnpm", &["store", "path"])
        .and_then(|p| Path::new(&p).parent().map(Path::to_path_buf))
        .unwrap_or(default)
}
