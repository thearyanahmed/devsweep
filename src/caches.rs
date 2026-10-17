use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::Serialize;

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
    let docker = on_path("docker").then(|| thread::spawn(docker_reclaimable));
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

fn output(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// `pnpm store path` names one store version (…/store/v10); measure every version beside it.
fn pnpm_store(default: PathBuf) -> PathBuf {
    output("pnpm", &["store", "path"])
        .and_then(|p| Path::new(&p).parent().map(Path::to_path_buf))
        .unwrap_or(default)
}

/// What `docker system df` says could be reclaimed; 0 when Docker isn't running.
fn docker_reclaimable() -> u64 {
    let (tx, rx) = mpsc::channel();
    // Nobody may be listening by the time it answers; then the send just fails.
    thread::spawn(move || {
        tx.send(output(
            "docker",
            &["system", "df", "--format", "{{.Reclaimable}}"],
        ))
    });
    // A stopped Docker Desktop can hang the CLI; don't wait on it.
    let Ok(Some(out)) = rx.recv_timeout(Duration::from_secs(5)) else {
        return 0;
    };
    out.lines().filter_map(|l| docker_size(l.trim())).sum()
}

/// Parses the leading size of a line like "1.2GB (40%)", in Docker's decimal units.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "parsed from digits only, so never negative"
)]
fn docker_size(s: &str) -> Option<u64> {
    let number = |c: char| c.is_ascii_digit() || c == '.';
    let digits: String = s.chars().take_while(|&c| number(c)).collect();
    let v: f64 = digits.parse().ok()?;
    let unit = s
        .trim_start_matches(number)
        .trim_start()
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()?;
    let mult = match unit {
        "B" => 1.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => return None,
    };
    Some((v * mult) as u64)
}

/// Whether `cmd` is an executable on PATH.
pub fn on_path(cmd: &str) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|d| {
            std::fs::metadata(d.join(cmd))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::docker_size;

    #[test]
    fn docker_sizes() {
        assert_eq!(docker_size("1.2GB (40%)"), Some(1_200_000_000));
        assert_eq!(docker_size("0B"), Some(0));
        assert_eq!(docker_size("512kB"), Some(512_000));
        assert_eq!(docker_size(""), None);
    }
}
