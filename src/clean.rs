use std::collections::HashMap;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Options;
use crate::caches::Cache;
use crate::cmd::on_path;
use crate::output::{bold, dim, human, paint, plural, rel, tilde};
use crate::rules::{Safety, exists};
use crate::scan::{Artifact, measure};

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

    fn run(&self) -> Result<(), String> {
        if self.argv.is_empty() {
            for p in &self.paths {
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
    /// Caution, with --include-caution: asked about one by one.
    one_by_one: Vec<Job>,
    /// No tool cleans these and -rm wasn't given.
    need_rm: Vec<&'a Artifact>,
    /// Caution, without --include-caution.
    caution: Vec<&'a Artifact>,
    /// Tracked in git: never cleaned.
    tracked: Vec<&'a Artifact>,
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
            Safety::Unsafe => {
                p.tracked.push(a);
                continue;
            }
            _ if a.clean.is_none() && !o.rm => {
                p.need_rm.push(a);
                continue;
            }
            Safety::Caution if !o.caution => {
                p.caution.push(a);
                continue;
            }
            Safety::Caution => false,
            Safety::Safe => true,
        };
        if let Some(argv) = &a.clean
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
        if let Some(argv) = &a.clean {
            let slot = Slot {
                in_batch: to_batch,
                index: list.len(),
            };
            by_cmd.insert((a.project.clone(), argv.clone()), slot);
        }
        list.push(Job {
            label: rel(root, &a.path),
            dir: a.project.clone(),
            argv: a.clean.clone().unwrap_or_default(),
            paths: vec![a.path.clone()],
            before: a.bytes,
        });
    }
    p
}

/// Runs jobs, or with --dry-run only says what it would run, and keeps the score.
struct Runner {
    dry_run: Option<bool>,
    freed: u64,
    would_free: u64,
    /// An rm -rf was left as a dry run only because --dry-run wasn't given.
    rm_held_back: bool,
    failed: bool,
}

impl Runner {
    /// Whether a job only shows what it would do: as --dry-run says, and when it doesn't say, a tool's own command
    /// runs while an rm -rf, which nothing can undo, stays a dry run.
    fn dry(&self, j: &Job) -> bool {
        self.dry_run.unwrap_or(j.argv.is_empty())
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
        if let Err(e) = j.run() {
            eprintln!("  {} {}: {e}", paint("31", "failed"), j.label);
            self.failed = true;
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

pub fn run_clean(roots: &[PathBuf], arts: &[&Artifact], caches: &[Cache], o: &Options) -> i32 {
    let root = &roots[0];
    let p = plan(root, arts, o);
    let mut runner = Runner {
        dry_run: o.dry_run,
        freed: 0,
        would_free: 0,
        rm_held_back: false,
        failed: false,
    };

    if !p.batch.is_empty() {
        let total: u64 = p.batch.iter().map(|j| j.before).sum();
        println!(
            "{}  {}, {}\n",
            paint("32", "Safe to clean"),
            plural(p.batch.len(), "job"),
            human(total)
        );
        for j in &p.batch {
            println!(
                "  {:<10}  {}  {}",
                human(j.before),
                j.label,
                dim(&format!("via {}{}", j.method(), j.rm_target()))
            );
        }
        let question = format!("\nRun these {} ({})?", p.batch.len(), human(total));
        if o.yes || p.batch.iter().all(|j| runner.dry(j)) || ask(&question) {
            p.batch.iter().for_each(|j| runner.run(j));
        }
    }
    if !p.one_by_one.is_empty() {
        println!("\n{}  asked one by one", paint("33", "Caution"));
        for j in &p.one_by_one {
            let question = format!(
                "{}{}  {} ({})?",
                j.method(),
                j.rm_target(),
                j.label,
                human(j.before)
            );
            if runner.dry(j) || ask(&question) {
                runner.run(j);
            }
        }
    }
    if !caches.is_empty() {
        clean_caches(caches, o, &mut runner);
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

/// Offers each shared cache in turn.
fn clean_caches(caches: &[Cache], o: &Options, runner: &mut Runner) {
    println!("\n{}  asked one by one", bold("Shared caches"));
    for c in caches {
        if c.cmd.is_none() && !o.rm {
            let why = format!("(no clean command; -rm to rm -rf {})", tilde(&c.path));
            println!("  {}  {}  {}", dim("skipped"), c.name, dim(&why));
            continue;
        }
        let argv = c.cmd.clone().unwrap_or_default();
        let j = Job {
            label: c.name.into(),
            dir: if argv.is_empty() {
                c.path.clone()
            } else {
                PathBuf::new()
            },
            paths: if c.path.as_os_str().is_empty() {
                vec![]
            } else {
                vec![c.path.clone()]
            },
            argv,
            before: c.bytes,
        };
        let note = if c.note.is_empty() {
            String::new()
        } else {
            dim(&format!("  {}", c.note))
        };
        let question = format!(
            "{}  {} {}?{note}",
            c.name,
            human(c.bytes),
            dim(&format!("via {}", c.method()))
        );
        if runner.dry(&j) || ask(&question) {
            runner.run(&j);
        }
    }
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
            "\n{}  {} left alone; add --include-caution to be asked about each",
            paint("33", "Caution"),
            plural(p.caution.len(), "artifact")
        );
    }
    if !p.tracked.is_empty() {
        println!(
            "\n{}  {} tracked in git, never cleaned",
            paint("31", "Unsafe"),
            plural(p.tracked.len(), "artifact")
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
