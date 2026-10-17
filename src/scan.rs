use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::SystemTime;

use serde::{Serialize, Serializer};

use crate::output::plural;
use crate::rules::{self, Safety, exists};

/// One directory of build output that some project claims.
#[derive(Serialize)]
pub struct Artifact {
    pub path: PathBuf,
    pub project: PathBuf,
    pub kind: &'static str,
    pub dir: String,
    /// Disk space used.
    pub bytes: u64,
    /// Part of `bytes` in hard links also used elsewhere (pnpm store): not freed by deleting.
    #[serde(rename = "shared_bytes")]
    pub shared: u64,
    pub files: u64,
    #[serde(serialize_with = "rfc3339")]
    pub last_built: Option<SystemTime>,
    pub safety: Safety,
    pub why: String,
    pub rebuild: String,
    /// The tool's own command, run in `project`; `None` means only rm -rf removes it.
    #[serde(rename = "clean_cmd")]
    pub clean: Option<Vec<String>>,
}

impl Artifact {
    /// What deleting the artifact gives back.
    pub fn freed(&self) -> u64 {
        self.bytes - self.shared
    }

    /// How clean removes the artifact, for display.
    pub fn method(&self) -> String {
        match &self.clean {
            Some(argv) => argv.join(" "),
            None => format!("rm -rf {}", self.dir),
        }
    }
}

pub fn scan(roots: &[PathBuf]) -> Vec<Artifact> {
    let mut found = Vec::new();
    for root in roots {
        find(root, &mut found);
    }

    // Measuring is most of the time (node_modules has a lot of files): spread it over the cores.
    let slots: Vec<Mutex<Artifact>> = found.into_iter().map(Mutex::new).collect();
    let next = AtomicUsize::new(0);
    let workers = thread::available_parallelism().map_or(4, |n| n.get());
    thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                while let Some(slot) = slots.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let mut a = slot.lock().unwrap();
                    let m = measure(&a.path);
                    (a.bytes, a.shared, a.files, a.last_built) =
                        (m.bytes, m.shared, m.files, m.last_built);
                    check_git(&mut a);
                }
            });
        }
    });
    slots.into_iter().map(|m| m.into_inner().unwrap()).collect()
}

/// Walks `dir` in name order, collecting artifacts without descending into them.
fn find(dir: &Path, found: &mut Vec<Artifact>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return; // unreadable: skip, don't abort the scan
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let path = e.path();
        let Some(r) = rules::find(&path, dir, &name) else {
            find(&path, found);
            continue;
        };
        let (rebuild, safety, why) = r.rebuild.resolve(dir);
        let clean = r.clean.resolve(dir, &name);
        found.push(Artifact {
            path,
            project: dir.to_path_buf(),
            kind: r.kind,
            dir: name,
            bytes: 0,
            shared: 0,
            files: 0,
            last_built: None,
            safety: r.safety.max(safety),
            why,
            rebuild,
            clean,
        });
    }
}

#[derive(Default)]
pub struct Measure {
    pub bytes: u64,
    pub shared: u64,
    pub files: u64,
    pub last_built: Option<SystemTime>,
}

/// Size, file count and newest modification time. A hard-linked file counts once, and counts as shared only when
/// some of its links live outside `root` (deleting it doesn't free it then).
pub fn measure(root: &Path) -> Measure {
    struct Inode {
        size: u64,
        nlink: u64,
        seen: u64,
    }
    fn walk(p: &Path, m: &mut Measure, links: &mut HashMap<u64, Inode>) {
        let Ok(md) = fs::symlink_metadata(p) else {
            return;
        };
        if let Ok(t) = md.modified()
            && m.last_built.is_none_or(|last| t > last)
        {
            m.last_built = Some(t);
        }
        let size = md.blocks() * 512;
        let is_dir = md.is_dir();
        let mut counted = true;
        if !is_dir && md.nlink() > 1 {
            match links.get_mut(&md.ino()) {
                Some(n) => {
                    n.seen += 1;
                    counted = false;
                }
                None => {
                    links.insert(
                        md.ino(),
                        Inode {
                            size,
                            nlink: md.nlink(),
                            seen: 1,
                        },
                    );
                }
            }
        }
        if counted {
            m.bytes += size;
            if !is_dir {
                m.files += 1;
            }
        }
        if is_dir && let Ok(entries) = fs::read_dir(p) {
            for e in entries.flatten() {
                walk(&e.path(), m, links);
            }
        }
    }

    let mut m = Measure::default();
    let mut links = HashMap::new();
    walk(root, &mut m, &mut links);
    m.shared = links
        .values()
        .filter(|n| n.seen < n.nlink)
        .map(|n| n.size)
        .sum();
    m
}

/// Downgrades an artifact whose contents git tracks, and notes one that isn't gitignored.
fn check_git(a: &mut Artifact) {
    if !a.project.ancestors().any(|d| exists(&d.join(".git"))) {
        return;
    }
    let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(&a.project)
        .args(["ls-files", "--", &a.dir])
        .stderr(Stdio::null())
        .output()
    else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let tracked = out.stdout.iter().filter(|&&b| b == b'\n').count();
    if tracked > 0 {
        a.safety = Safety::Unsafe;
        a.why = format!("{} tracked in git", plural(tracked, "file"));
        return;
    }
    let ignored = Command::new("git")
        .arg("-C")
        .arg(&a.project)
        .args(["check-ignore", "-q", &a.dir])
        .status()
        .is_ok_and(|s| s.success());
    if !ignored {
        a.safety = a.safety.max(Safety::Caution);
        a.why = join(&a.why, "not gitignored");
    }
}

fn join(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.into()
    } else {
        format!("{a}; {b}")
    }
}

/// RFC 3339 in UTC, with the fraction trimmed the way Go's RFC3339Nano does.
fn rfc3339<S: Serializer>(t: &Option<SystemTime>, s: S) -> Result<S::Ok, S::Error> {
    let Some(t) = t else {
        return s.serialize_none();
    };
    let d = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let mut out = format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    );
    if d.subsec_nanos() > 0 {
        out += format!(".{:09}", d.subsec_nanos()).trim_end_matches('0');
    }
    out.push('Z');
    s.serialize_str(&out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn safety() {
        let root = std::env::temp_dir().join(format!("devsweep-test-{}", std::process::id()));
        let _cleanup = TempDir(root.clone());
        let write = |p: &str, body: &str| {
            let p = root.join(p);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        };
        let git = |dir: &str, args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(root.join(dir))
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        // Rust, gitignored target: safe.
        write("rs/Cargo.toml", "");
        write("rs/.gitignore", "target\n");
        write("rs/target/debug/app", "binary");
        git("rs", &["init", "-q"]);

        // Node with a lockfile but node_modules not gitignored: caution. dist tracked in git: unsafe.
        write("web/package.json", "{}");
        write("web/package-lock.json", "{}");
        write("web/node_modules/x/index.js", "x");
        write("web/dist/logo.svg", "<svg/>");
        git("web", &["init", "-q"]);
        git("web", &["add", "dist"]);

        // Node without a lockfile, no git: caution.
        write("loose/package.json", "{}");
        write("loose/node_modules/y/index.js", "y");

        // A directory named target with no Cargo.toml is not claimed.
        write("docs/target/notes.md", "keep me");

        let got: HashMap<String, Safety> = scan(std::slice::from_ref(&root))
            .into_iter()
            .map(|a| {
                (
                    a.path
                        .strip_prefix(&root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    a.safety,
                )
            })
            .collect();
        let want = HashMap::from([
            ("rs/target".to_string(), Safety::Safe),
            ("web/node_modules".to_string(), Safety::Caution),
            ("web/dist".to_string(), Safety::Unsafe),
            ("loose/node_modules".to_string(), Safety::Caution),
        ]);
        assert_eq!(got, want);
    }

    #[test]
    fn rfc3339_matches_go() {
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::new(1_792_252_466, 500_000_000);
        let mut out = Vec::new();
        rfc3339(&Some(t), &mut serde_json::Serializer::new(&mut out)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\"2026-10-17T15:54:26.5Z\""
        );
    }
}
