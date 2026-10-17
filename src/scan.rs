use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt as _;
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
    pub const fn freed(&self) -> u64 {
        self.bytes - self.shared
    }

    /// How clean removes the artifact, for display.
    pub fn method(&self) -> String {
        self.clean
            .as_ref()
            .map_or_else(|| format!("rm -rf {}", self.dir), |argv| argv.join(" "))
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
        let measures = measure_all(&paths);
        (measures, git.join().expect("the git thread panicked"))
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
    thread::available_parallelism().map_or(4, std::num::NonZero::get)
}

/// `f` over every item, on all cores, keeping the order.
fn parallel_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let mut done: Vec<(usize, R)> = thread::scope(|s| {
        let handles: Vec<_> = (0..workers().min(items.len()))
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else {
                            return mine;
                        };
                        mine.push((i, f(item)));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a worker thread panicked"))
            .collect()
    });
    done.sort_unstable_by_key(|&(i, _)| i);
    done.into_iter().map(|(_, r)| r).collect()
}

/// Walks `dir` in name order, collecting artifacts without descending into them.
fn find(dir: &Path, found: &mut Vec<Artifact>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return; // unreadable: skip, don't abort the scan
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(fs::DirEntry::file_name);
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

/// A hard-linked file, counted once however many of its links a walk meets.
#[derive(Debug, Clone, Copy)]
struct Link {
    size: u64,
    nlink: u64,
    seen: u64,
}

/// What one worker saw of one tree. Hard-linked files are kept aside by inode, so the merge can count each once.
#[derive(Debug, Default)]
struct Partial {
    bytes: u64,
    files: u64,
    last_built: Option<SystemTime>,
    links: HashMap<u64, Link>,
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
            let nlink = md.nlink();
            self.links
                .entry(md.ino())
                .or_insert(Link {
                    size,
                    nlink,
                    seen: 0,
                })
                .seen += 1;
        } else {
            self.bytes += size;
            self.files += 1;
        }
    }
}

/// A directory waiting to be read, and which tree it belongs to.
#[derive(Debug)]
struct Pending {
    tree: usize,
    dir: PathBuf,
    /// The tree's own root, which nothing has looked at yet.
    is_root: bool,
}

/// The directories still to read, shared by the workers.
#[derive(Debug)]
struct DirQueue {
    state: Mutex<QueueState>,
    wake: Condvar,
}

#[derive(Debug)]
struct QueueState {
    waiting: Vec<Pending>,
    /// Directories waiting or being read: the walk is over when this reaches 0.
    open: usize,
}

impl DirQueue {
    fn new(roots: &[&Path]) -> Self {
        let waiting: Vec<Pending> = roots
            .iter()
            .enumerate()
            .map(|(tree, p)| Pending {
                tree,
                dir: p.to_path_buf(),
                is_root: true,
            })
            .collect();
        let open = waiting.len();
        Self {
            state: Mutex::new(QueueState { waiting, open }),
            wake: Condvar::new(),
        }
    }

    /// The next directory to read, waiting while others are being read; `None` once the walk is over.
    fn take(&self) -> Option<Pending> {
        let mut q = self.state.lock().expect("a measuring thread panicked");
        loop {
            if let Some(p) = q.waiting.pop() {
                return Some(p);
            }
            if q.open == 0 {
                return None;
            }
            q = self.wake.wait(q).expect("a measuring thread panicked");
        }
    }

    /// Marks a directory read, queueing the subdirectories found in it.
    fn finish(&self, subdirs: Vec<Pending>) {
        let mut q = self.state.lock().expect("a measuring thread panicked");
        q.open = q.open + subdirs.len() - 1;
        let more = !subdirs.is_empty();
        q.waiting.extend(subdirs);
        if more || q.open == 0 {
            self.wake.notify_all();
        }
    }
}

/// Measures several trees at once with one pool of threads sharing a queue of directories, so a single huge tree
/// (`node_modules`) is spread over every core too. A hard-linked file counts once, and counts as shared only when some
/// of its links live outside its tree (deleting the tree doesn't free it then).
pub fn measure_all(roots: &[&Path]) -> Vec<Measure> {
    let queue = DirQueue::new(roots);
    let partials: Vec<Vec<Partial>> = thread::scope(|s| {
        let handles: Vec<_> = (0..workers())
            .map(|_| s.spawn(|| walk(&queue, roots.len())))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("a measuring thread panicked"))
            .collect()
    });
    (0..roots.len())
        .map(|tree| merge(&partials, tree))
        .collect()
}

/// One worker: reads directories off the queue until the walk is over, tallying what it sees per tree.
fn walk(queue: &DirQueue, trees: usize) -> Vec<Partial> {
    let mut mine: Vec<Partial> = (0..trees).map(|_| Partial::default()).collect();
    while let Some(Pending { tree, dir, is_root }) = queue.take() {
        let mut subdirs = Vec::new();
        let read = !is_root
            || fs::symlink_metadata(&dir).is_ok_and(|md| {
                mine[tree].add(&md);
                md.is_dir()
            });
        if read && let Ok(entries) = fs::read_dir(&dir) {
            for e in entries.flatten() {
                // DirEntry::metadata doesn't follow symlinks, and is cheaper than a stat by path.
                let Ok(md) = e.metadata() else { continue };
                mine[tree].add(&md);
                if md.is_dir() {
                    subdirs.push(Pending {
                        tree,
                        dir: e.path(),
                        is_root: false,
                    });
                }
            }
        }
        queue.finish(subdirs);
    }
    mine
}

/// One tree's total from every worker's part of it.
fn merge(partials: &[Vec<Partial>], tree: usize) -> Measure {
    let mut m = Measure::default();
    let mut links: HashMap<u64, Link> = HashMap::new();
    for p in partials.iter().map(|w| &w[tree]) {
        m.bytes += p.bytes;
        m.files += p.files;
        m.last_built = m.last_built.max(p.last_built);
        for (&ino, &link) in &p.links {
            links.entry(ino).or_insert(Link { seen: 0, ..link }).seen += link.seen;
        }
    }
    for link in links.into_values() {
        m.bytes += link.size;
        m.files += 1;
        if link.seen < link.nlink {
            m.shared += link.size;
        }
    }
    m
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
    #[expect(clippy::naive_bytecount, reason = "one count over a short git listing")]
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

/// RFC 3339 in UTC, with the fraction trimmed the way Go's `RFC3339Nano` does.
#[expect(
    clippy::ref_option,
    reason = "serde's serialize_with passes the field by reference"
)]
fn rfc3339<S: Serializer>(t: &Option<SystemTime>, s: S) -> Result<S::Ok, S::Error> {
    let Some(t) = t else {
        return s.serialize_none();
    };
    let d = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let secs = i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
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
            if let Err(e) = fs::remove_dir_all(&self.0) {
                eprintln!("couldn't remove {}: {e}", self.0.display());
            }
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
            ("rs/target".to_owned(), Safety::Safe),
            ("web/node_modules".to_owned(), Safety::Caution),
            ("web/dist".to_owned(), Safety::Unsafe),
            ("loose/node_modules".to_owned(), Safety::Caution),
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
