use std::collections::HashMap;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cli::Options;
use crate::cmd::on_path;
use crate::output::{bold, dim, human, item_method, paint, plural, rel, tilde};
use crate::scan::{Artifact, measure};
use crate::sweep::{Clean, Item, Safety, Section, exists};

/// One thing clean will do: a tool command run in a directory, or an rm -rf.
struct Job {
    label: String,
    /// Where the command runs; empty means the current directory.
    dir: PathBuf,
    /// Empty: rm -rf `paths`.
    argv: Vec<String>,
    /// What it should free; measured before and after.
    paths: Vec<PathBuf>,
    before: u64,
}

impl Job {
    fn method(&self) -> String {
        if self.argv.is_empty() {
            "rm -rf".into()
        } else {
            self.argv.join(" ")
        }
    }

    /// The paths an rm -rf will remove, for display; nothing for a tool command.
    fn rm_target(&self) -> String {
        if !self.argv.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = self.paths.iter().map(|p| tilde(p)).collect();
        format!(" {}", parts.join(" "))
    }

    fn run(&self, bases: &[PathBuf]) -> Result<(), String> {
        if self.argv.is_empty() {
            for p in &self.paths {
                removable(p, bases)?;
                std::fs::remove_dir_all(p).map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        if !on_path(&self.argv[0]) {
            return Err(format!("{} isn't installed", self.argv[0]));
        }
        println!("  {} {}", dim("$"), dim(&self.method()));
        let mut cmd = Command::new(&self.argv[0]);
        cmd.args(&self.argv[1..]);
        if !self.dir.as_os_str().is_empty() {
            cmd.current_dir(&self.dir);
        }
        match cmd.status() {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(s.to_string()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// What clean will do with the artifacts it was given.
#[derive(Default)]
struct Plan<'a> {
    /// Safe: asked about once, together.
    batch: Vec<Job>,
    /// Caution, with -upto caution or unsafe: asked about one by one.
    one_by_one: Vec<Job>,
    /// Tracked in git, with -upto unsafe: asked about one by one, each confirmed by typing its name.
    tracked_jobs: Vec<Job>,
    /// No tool cleans these and -rm wasn't given.
    need_rm: Vec<&'a Artifact>,
    /// Caution, below -upto caution.
    caution: Vec<&'a Artifact>,
    /// Tracked in git, below -upto unsafe.
    tracked: Vec<&'a Artifact>,
    /// A sweeper said they can't be removed now.
    blocked: Vec<&'a Artifact>,
}

/// A clean command and the project it runs in.
type CmdKey = (PathBuf, Vec<String>);

/// Where a job sits in the plan.
#[derive(Debug, Clone, Copy)]
struct Slot {
    in_batch: bool,
    index: usize,
}

fn plan<'a>(root: &Path, arts: &[&'a Artifact], o: &Options) -> Plan<'a> {
    let mut p = Plan::default();
    // Several artifacts of one project can share a command (cargo clean in a workspace): run it once.
    let mut by_cmd: HashMap<CmdKey, Slot> = HashMap::new();
    for &a in arts {
        let to_batch = match a.safety {
            Safety::Unsafe if o.upto < Safety::Unsafe => {
                p.tracked.push(a);
                continue;
            }
            _ if matches!(a.clean, Clean::Blocked(_)) => {
                p.blocked.push(a);
                continue;
            }
            _ if a.clean == Clean::RemoveDir && !o.rm => {
                p.need_rm.push(a);
                continue;
            }
            Safety::Caution if o.upto < Safety::Caution => {
                p.caution.push(a);
                continue;
            }
            Safety::Caution => false,
            Safety::Safe => true,
            Safety::Unsafe => {
                p.tracked_jobs.push(job(root, a));
                continue;
            }
        };
        let argv = job(root, a).argv;
        if !argv.is_empty()
            && let Some(&Slot { in_batch, index }) = by_cmd.get(&(a.project.clone(), argv.clone()))
        {
            let j = if in_batch {
                &mut p.batch[index]
            } else {
                &mut p.one_by_one[index]
            };
            j.paths.push(a.path.clone());
            j.before += a.bytes;
            continue;
        }
        let list = if to_batch {
            &mut p.batch
        } else {
            &mut p.one_by_one
        };
        if !argv.is_empty() {
            let slot = Slot {
                in_batch: to_batch,
                index: list.len(),
            };
            by_cmd.insert((a.project.clone(), argv.clone()), slot);
        }
        list.push(job(root, a));
    }
    p
}

/// Cleaning one artifact on its own.
fn job(root: &Path, a: &Artifact) -> Job {
    Job {
        label: rel(root, &a.path),
        dir: a.project.clone(),
        argv: match &a.clean {
            Clean::Tool(argv) => argv.clone(),
            Clean::RemoveDir | Clean::Blocked(_) => Vec::new(),
        },
        paths: vec![a.path.clone()],
        before: a.bytes,
    }
}

/// Runs jobs, or with --dry-run only says what it would run, and keeps the score.
struct Runner {
    dry_run: Option<bool>,
    /// The folders rm -rf may delete inside: the scanned roots and the home folder.
    bases: Vec<PathBuf>,
    freed: u64,
    would_free: u64,
    /// An rm -rf was left as a dry run only because --dry-run wasn't given.
    rm_held_back: bool,
    failed: bool,
    /// -y: every question is answered yes.
    yes: bool,
}

impl Runner {
    /// Whether a job only shows what it would do: as --dry-run says, and when it doesn't say, a tool's own command
    /// runs while an rm -rf, which nothing can undo, stays a dry run.
    fn dry(&self, j: &Job) -> bool {
        self.dry_run.unwrap_or(j.argv.is_empty())
    }

    /// Asks a yes/no question, or with -y shows it answered.
    fn ask(&self, q: &str) -> bool {
        if self.yes {
            println!("{q} [y/N] y");
            return true;
        }
        ask(q)
    }

    fn run(&mut self, j: &Job) {
        if self.dry(j) {
            self.rm_held_back |= self.dry_run.is_none();
            let wh = if j.dir.as_os_str().is_empty() || j.argv.is_empty() {
                String::new()
            } else {
                format!("{}  ", dim(&format!("(cd {})", tilde(&j.dir))))
            };
            println!("  would run  {wh}{}{}", j.method(), j.rm_target());
            self.would_free += j.before;
            return;
        }
        if let Err(e) = j.run(&self.bases) {
            eprintln!("  {} {}: {e}", paint("31", "failed"), j.label);
            self.failed = true;
            return;
        }
        let got = j.before.saturating_sub(size_of(&j.paths));
        self.freed += got;
        println!(
            "  {}  {}  freed {}",
            paint("32", "done"),
            j.label,
            human(got)
        );
    }
}

pub(crate) fn run_clean(
    roots: &[PathBuf],
    arts: &[&Artifact],
    offered: &[(Section, Vec<&Item>)],
    o: &Options,
) -> i32 {
    // Labels are relative to the first folder scanned; with none, to the home folder.
    let root = &roots.first().cloned().or_else(home).unwrap_or_default();
    let p = plan(root, arts, o);
    let mut runner = Runner {
        dry_run: o.dry_run,
        bases: roots.iter().cloned().chain(home()).collect(),
        freed: 0,
        would_free: 0,
        rm_held_back: false,
        failed: false,
        yes: o.yes,
    };

    // Every rm -rf waits for a step of its own at the end, after all the tools' own commands.
    let (batch, mut rm): (Vec<&Job>, Vec<&Job>) = p.batch.iter().partition(|j| !j.argv.is_empty());
    let (one_by_one, rm_caution): (Vec<&Job>, Vec<&Job>) =
        p.one_by_one.iter().partition(|j| !j.argv.is_empty());
    rm.extend(rm_caution);
    if !batch.is_empty() {
        let total: u64 = batch.iter().map(|j| j.before).sum();
        println!(
            "{}  {}, {}\n",
            paint("32", "Safe to clean"),
            plural(batch.len(), "job"),
            human(total)
        );
        for j in &batch {
            println!(
                "  {:<10}  {}  {}",
                human(j.before),
                j.label,
                dim(&format!("via {}{}", j.method(), j.rm_target()))
            );
        }
        let question = format!("\nRun {} ({})?", plural(batch.len(), "job"), human(total));
        if batch.iter().all(|j| runner.dry(j)) || runner.ask(&question) {
            for j in &batch {
                runner.run(j);
            }
        }
    }
    if !one_by_one.is_empty() {
        println!("\n{}  asked one by one", paint("33", "Caution"));
        for j in &one_by_one {
            let question = format!(
                "{}{}  {} ({})?",
                j.method(),
                j.rm_target(),
                j.label,
                human(j.before)
            );
            if runner.dry(j) || runner.ask(&question) {
                runner.run(j);
            }
        }
    }
    let mut rm_items = Vec::new();
    for (sec, items) in offered {
        clean_items(sec, items, o, &mut runner, &mut rm_items);
    }
    rm.extend(rm_items.iter());
    if !rm.is_empty() {
        remove_dirs(&rm, &mut runner);
    }
    if !p.tracked_jobs.is_empty() {
        clean_tracked(&p.tracked_jobs, &mut runner);
    }
    report_left_alone(root, &p);

    println!();
    if runner.freed > 0 || runner.would_free == 0 {
        println!("Freed {}", bold(&human(runner.freed)));
    }
    if runner.would_free > 0 {
        println!("Would free {}", bold(&human(runner.would_free)));
    }
    if runner.rm_held_back {
        println!(
            "{}",
            dim("rm -rf only showed what it would delete; add --dry-run=false to delete for real")
        );
    }
    i32::from(runner.failed)
}

/// Offers a section's items one by one. Ones a sweeper says are blocked are only named, with what's in the way; ones
/// only rm -rf removes wait for -rm.
fn clean_items(
    sec: &Section,
    items: &[&Item],
    o: &Options,
    runner: &mut Runner,
    rm: &mut Vec<Job>,
) {
    println!("\n{}  asked one by one", bold(sec.title));
    for i in items {
        let path = i.path.clone();
        let (argv, dir) = match &i.clean {
            Clean::Blocked(blocker) => {
                let why = format!("({}: {blocker})", i.why);
                println!("  {}  {}  {}", dim("skipped"), i.label, dim(&why));
                continue;
            }
            Clean::RemoveDir if !o.rm => {
                let why = format!(
                    "(no clean command; -rm to rm -rf {})",
                    path.as_deref().map(tilde).unwrap_or_default()
                );
                println!("  {}  {}  {}", dim("skipped"), i.label, dim(&why));
                continue;
            }
            // rm -rf removes the item's own folder and nothing else.
            Clean::RemoveDir => match &path {
                Some(p) => (Vec::new(), p.clone()),
                None => continue,
            },
            Clean::Tool(argv) => (argv.clone(), PathBuf::new()),
        };
        let j = Job {
            label: i.label.clone(),
            dir,
            argv,
            paths: path.into_iter().collect(),
            before: i.bytes.unwrap_or(0),
        };
        let extra = if i.note.is_empty() { &i.why } else { &i.note };
        let extra = if extra.is_empty() {
            String::new()
        } else {
            dim(&format!("  {extra}"))
        };
        let question = format!(
            "{}  {} {}?{extra}",
            i.label,
            human(j.before),
            dim(&format!("via {}", item_method(i)))
        );
        if j.argv.is_empty() {
            rm.push(j);
        } else if runner.dry(&j) || runner.ask(&question) {
            runner.run(&j);
        }
    }
}

/// The last step: every folder only rm -rf removes, listed together and asked about once. Without --dry-run=false it
/// only shows what it would delete.
fn remove_dirs(jobs: &[&Job], runner: &mut Runner) {
    let total: u64 = jobs.iter().map(|j| j.before).sum();
    println!(
        "\n{}  {}, {}  {}",
        paint("35", "rm -rf"),
        plural(jobs.len(), "folder"),
        human(total),
        dim("(no tool removes these)")
    );
    if jobs.iter().all(|j| runner.dry(j)) {
        for j in jobs {
            runner.run(j);
        }
        return;
    }
    for j in jobs {
        println!("  {:<10}  {}", human(j.before), dim(j.rm_target().trim()));
    }
    let question = format!(
        "\nDelete {} for good ({})?",
        plural(jobs.len(), "folder"),
        human(total)
    );
    if runner.ask(&question) {
        for j in jobs {
            runner.run(j);
        }
    }
}

/// The last step, with -upto unsafe: what git tracks files in, one by one, each confirmed by typing its folder's name.
/// One with uncommitted changes is refused, so that git can always bring back what was deleted.
fn clean_tracked(jobs: &[Job], runner: &mut Runner) {
    println!(
        "\n{}  {}",
        paint("31", "Unsafe"),
        dim(
            "git tracks files in these: type each one's name to clean it; git checkout brings it back"
        )
    );
    for j in jobs {
        if let Some(why) = uncommitted(j) {
            println!(
                "  {}  {}  {}",
                dim("skipped"),
                j.label,
                dim(&format!("({why})"))
            );
            continue;
        }
        if runner.dry(j) {
            runner.run(j);
            continue;
        }
        let name = j.paths[0]
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let question = format!(
            "{}{}  {} ({}): type {} to confirm",
            j.method(),
            j.rm_target(),
            j.label,
            human(j.before),
            bold(&name)
        );
        if prompt_word(&question, &name).unwrap_or(false) {
            runner.run(j);
        }
    }
}

/// Why a tracked artifact can't be cleaned now: changes git hasn't committed, or git couldn't say.
fn uncommitted(j: &Job) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(&j.dir)
        .args(["status", "--porcelain", "--untracked-files=all", "--"])
        .args(&j.paths)
        .output();
    match out {
        Ok(o) if o.status.success() && o.stdout.is_empty() => None,
        Ok(o) if o.status.success() => {
            Some("uncommitted changes in it: commit or stash them first".into())
        }
        _ => Some("git couldn't say whether it has uncommitted changes".into()),
    }
}

/// Whether rm -rf may delete `path`: a real folder, not a symlink, named without `..`, strictly inside one of `bases`
/// and none of them itself, nor `/` or the home folder. Checked right before deleting, whatever claimed it.
fn removable(path: &Path, bases: &[PathBuf]) -> Result<(), String> {
    let shown = path.display();
    if !path.is_absolute()
        || path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    {
        return Err(format!(
            "refusing to rm -rf {shown}: not a plain absolute path"
        ));
    }
    let home = home();
    let mut protected = bases
        .iter()
        .map(PathBuf::as_path)
        .chain(home.as_deref())
        .chain([Path::new("/")]);
    if protected.any(|p| p == path) {
        return Err(format!(
            "refusing to rm -rf {shown}: it's a root, not something inside one"
        ));
    }
    if !bases.iter().any(|b| path.starts_with(b)) {
        return Err(format!(
            "refusing to rm -rf {shown}: it's outside the scanned folders and the home folder"
        ));
    }
    let md =
        std::fs::symlink_metadata(path).map_err(|e| format!("refusing to rm -rf {shown}: {e}"))?;
    if md.file_type().is_symlink() || !md.is_dir() {
        return Err(format!("refusing to rm -rf {shown}: not a real folder"));
    }
    Ok(())
}

/// The home folder, when there is one.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Refuses to clean as root, or with `/` or the home folder itself as a folder to scan.
pub(crate) fn preflight(roots: &[PathBuf]) -> Result<(), String> {
    let root_user = std::env::var_os("SUDO_UID").is_some()
        || crate::cmd::output("id", &["-u"]).is_some_and(|uid| uid == "0");
    if root_user {
        return Err("refusing to clean as root (or under sudo): run it as yourself".into());
    }
    let home = home();
    if let Some(r) = roots
        .iter()
        .find(|r| r.as_path() == Path::new("/") || Some(*r) == home.as_ref())
    {
        return Err(format!(
            "refusing to clean with {} as a folder to scan: name the folders that hold your projects",
            r.display()
        ));
    }
    Ok(())
}

/// Says what clean didn't touch, and the flag that would change that.
fn report_left_alone(root: &Path, p: &Plan<'_>) {
    if !p.need_rm.is_empty() {
        let total: u64 = p.need_rm.iter().map(|a| a.freed()).sum();
        println!(
            "\n{}  {} ({}) no tool cleans; add -rm to rm -rf:",
            dim("Left alone"),
            plural(p.need_rm.len(), "artifact"),
            human(total)
        );
        for a in &p.need_rm {
            println!("  {}  {}", human(a.freed()), dim(&rel(root, &a.path)));
        }
    }
    if !p.caution.is_empty() {
        println!(
            "\n{}  {} left alone; add -upto caution to be asked about each",
            paint("33", "Caution"),
            plural(p.caution.len(), "artifact")
        );
    }
    if !p.tracked.is_empty() {
        println!(
            "\n{}  {} tracked in git, left alone; add -upto unsafe to be asked about each",
            paint("31", "Unsafe"),
            plural(p.tracked.len(), "artifact")
        );
    }
    for a in &p.blocked {
        println!(
            "\n{}  {}  {}",
            dim("Blocked"),
            rel(root, &a.path),
            dim(&a.method())
        );
    }
}

fn size_of(paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .filter(|p| exists(p))
        .map(|p| measure(Path::new(p)).bytes)
        .sum()
}

/// Asks a yes/no question; anything but yes, including a closed stdin, is no.
fn ask(q: &str) -> bool {
    prompt(q).unwrap_or(false)
}

fn prompt(q: &str) -> io::Result<bool> {
    let mut out = io::stdout().lock();
    write!(out, "{q} [y/N] ")?;
    out.flush()?;
    drop(out);
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Asks for `word` to be typed; anything else, including a closed stdin, is no. -y never answers it.
fn prompt_word(q: &str, word: &str) -> io::Result<bool> {
    let mut out = io::stdout().lock();
    write!(out, "{q}: ")?;
    out.flush()?;
    drop(out);
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(!word.is_empty() && line.trim() == word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rm_rf_refuses_anything_but_a_real_folder_inside_a_root() {
        let base = std::env::temp_dir().join(format!("devsweep-rm-{}", std::process::id()));
        let inside = base.join("app/node_modules");
        fs_create(&inside);
        let outside = std::env::temp_dir().join(format!("devsweep-rm-out-{}", std::process::id()));
        fs_create(&outside);
        let link = base.join("app/link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let bases = vec![base.clone()];

        assert!(removable(&inside, &bases).is_ok());
        let refused = |p: &Path| removable(p, &bases).unwrap_err();
        assert!(refused(&base).contains("root"), "the scanned folder itself");
        assert!(refused(Path::new("/")).contains("root"));
        assert!(refused(&outside).contains("outside"));
        assert!(
            refused(&link).contains("not a real folder"),
            "a symlink, even to a folder"
        );
        assert!(refused(&base.join("app/../app/node_modules")).contains("plain"));
        assert!(
            refused(Path::new("app/node_modules")).contains("plain"),
            "a relative path"
        );
        assert!(refused(&base.join("app/missing")).contains("refusing"));

        std::fs::remove_dir_all(&base).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn clean_refuses_slash_and_home_as_roots() {
        assert!(preflight(&[PathBuf::from("/")]).is_err());
        if let Some(home) = home() {
            assert!(preflight(&[home.join("dev"), home]).is_err());
        }
    }

    fn fs_create(p: &Path) {
        std::fs::create_dir_all(p).unwrap();
    }
}
