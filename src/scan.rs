use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
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

    // Measuring is most of the time (node_modules has tens of thousands of files), and the git checks are
    // subprocesses: run both at once, each spread over the cores.
    let paths: Vec<&Path> = found.iter().map(|a| a.path.as_path()).collect();
    let (measures, verdicts) = thread::scope(|s| {
        let git = s.spawn(|| parallel_map(&found, git_verdict));
        (measure_all(&paths), git.join().unwrap())
    });
    for ((a, m), verdict) in found.iter_mut().zip(measures).zip(verdicts) {
        (a.bytes, a.shared, a.files, a.last_built) = (m.bytes, m.shared, m.files, m.last_built);
        if let Some((safety, why)) = verdict {
            a.safety = a.safety.max(safety);
            a.why = why;
        }
    }
    found
}

fn workers() -> usize {
    thread::available_parallelism().map_or(4, |n| n.get())
}

/// `f` over every item, on all cores, keeping the order.
fn parallel_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let out: Vec<Mutex<Option<R>>> = items.iter().map(|_| Mutex::new(None)).collect();
    thread::scope(|s| {
        for _ in 0..workers().min(items.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    *out[i].lock().unwrap() = Some(f(item));
                }
            });
        }
    });
    out.into_iter()
        .map(|m| m.into_inner().unwrap().unwrap())
        .collect()
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

/// Size, file count and newest modification time of one directory tree.
pub fn measure(root: &Path) -> Measure {
    measure_all(&[root]).pop().unwrap_or_default()
}

/// What one worker saw of one tree. Hard-linked files are kept aside by inode, so the merge can count each once.
#[derive(Default)]
struct Partial {
    bytes: u64,
    files: u64,
    last_built: Option<SystemTime>,
    /// inode -> (size, link count, links seen)
    links: HashMap<u64, (u64, u64, u64)>,
}

impl Partial {
    fn add(&mut self, md: &fs::Metadata) {
        if let Ok(t) = md.modified()
            && self.last_built.is_none_or(|last| t > last)
        {
            self.last_built = Some(t);
        }
        let size = md.blocks() * 512;
        if md.is_dir() {
            self.bytes += size;
        } else if md.nlink() > 1 {
            self.links
                .entry(md.ino())
                .or_insert((size, md.nlink(), 0))
                .2 += 1;
        } else {
            self.bytes += size;
            self.files += 1;
        }
    }
}

/// Measures several trees at once with one pool of threads sharing a queue of directories, so a single huge tree
/// (node_modules) is spread over every core too. A hard-linked file counts once, and counts as shared only when some
/// of its links live outside its tree (deleting the tree doesn't free it then).
pub fn measure_all(roots: &[&Path]) -> Vec<Measure> {
    // The queue holds (tree, directory, is the tree's root) and how many directories are queued or being read.
    let queue = Mutex::new((
        roots
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.to_path_buf(), true))
            .collect::<Vec<_>>(),
        roots.len(),
    ));
    let wake = Condvar::new();
    let partials: Vec<Vec<Partial>> = thread::scope(|s| {
        let handles: Vec<_> = (0..workers())
            .map(|_| {
                s.spawn(|| {
                    let mut mine: Vec<Partial> = roots.iter().map(|_| Partial::default()).collect();
                    loop {
                        let next = {
                            let mut q = queue.lock().unwrap();
                            loop {
                                if let Some(item) = q.0.pop() {
                                    break Some(item);
                                }
                                if q.1 == 0 {
                                    break None;
                                }
                                q = wake.wait(q).unwrap();
                            }
                        };
                        let Some((i, dir, is_root)) = next else {
                            return mine;
                        };
                        let mut subdirs = Vec::new();
                        let read = !is_root
                            || fs::symlink_metadata(&dir).is_ok_and(|md| {
                                mine[i].add(&md);
                                md.is_dir()
                            });
                        if read && let Ok(entries) = fs::read_dir(&dir) {
                            for e in entries.flatten() {
                                // DirEntry::metadata doesn't follow symlinks, and is cheaper than a stat by path.
                                let Ok(md) = e.metadata() else { continue };
                                mine[i].add(&md);
                                if md.is_dir() {
                                    subdirs.push((i, e.path(), false));
                                }
                            }
                        }
                        let mut q = queue.lock().unwrap();
                        q.1 += subdirs.len();
                        q.1 -= 1;
                        let more = !subdirs.is_empty();
                        q.0.extend(subdirs);
                        if more || q.1 == 0 {
                            wake.notify_all();
                        }
                    }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    (0..roots.len())
        .map(|i| {
            let mut m = Measure::default();
            let mut links: HashMap<u64, (u64, u64, u64)> = HashMap::new();
            for p in partials.iter().map(|w| &w[i]) {
                m.bytes += p.bytes;
                m.files += p.files;
                m.last_built = m.last_built.max(p.last_built);
                for (&ino, &(size, nlink, seen)) in &p.links {
                    links.entry(ino).or_insert((size, nlink, 0)).2 += seen;
                }
            }
            for (size, nlink, seen) in links.into_values() {
                m.bytes += size;
                m.files += 1;
                if seen < nlink {
                    m.shared += size;
                }
            }
            m
        })
        .collect()
}

/// Whether git changes an artifact's safety: unsafe when git tracks files in it, caution when it isn't gitignored.
fn git_verdict(a: &Artifact) -> Option<(Safety, String)> {
    if !a.project.ancestors().any(|d| exists(&d.join(".git"))) {
        return None;
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(&a.project)
        .args(["ls-files", "--", &a.dir])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let tracked = out.stdout.iter().filter(|&&b| b == b'\n').count();
    if tracked > 0 {
        return Some((
            Safety::Unsafe,
            format!("{} tracked in git", plural(tracked, "file")),
        ));
    }
    let ignored = Command::new("git")
        .arg("-C")
        .arg(&a.project)
        .args(["check-ignore", "-q", &a.dir])
        .status()
        .is_ok_and(|s| s.success());
    (!ignored).then(|| (Safety::Caution, join(&a.why, "not gitignored")))
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
