use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Options;
use crate::caches::{Cache, on_path};
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

pub fn run_clean(roots: &[PathBuf], arts: &[&Artifact], caches: &[Cache], o: &Options) -> i32 {
    let root = &roots[0];
    let (mut batch, mut one_by_one) = (Vec::new(), Vec::new());
    let (mut need_rm, mut caution, mut unsafe_) = (Vec::new(), Vec::new(), Vec::new());

    // Several artifacts of one project can share a command (cargo clean in a workspace): run it once.
    let mut by_cmd: HashMap<(PathBuf, Vec<String>), (bool, usize)> = HashMap::new();
    let mut add =
        |a: &Artifact, to_batch: bool, batch: &mut Vec<Job>, one_by_one: &mut Vec<Job>| {
            let list = if to_batch {
                &mut *batch
            } else {
                &mut *one_by_one
            };
            let Some(argv) = &a.clean else {
                list.push(Job {
                    label: rel(root, &a.path),
                    dir: a.project.clone(),
                    argv: vec![],
                    paths: vec![a.path.clone()],
                    before: a.bytes,
                });
                return;
            };
            let key = (a.project.clone(), argv.clone());
            if let Some(&(in_batch, i)) = by_cmd.get(&key) {
                let j = if in_batch {
                    &mut batch[i]
                } else {
                    &mut one_by_one[i]
                };
                j.paths.push(a.path.clone());
                j.before += a.bytes;
                return;
            }
            by_cmd.insert(key, (to_batch, list.len()));
            list.push(Job {
                label: rel(root, &a.path),
                dir: a.project.clone(),
                argv: argv.clone(),
                paths: vec![a.path.clone()],
                before: a.bytes,
            });
        };
    for &a in arts {
        match a.safety {
            Safety::Unsafe => unsafe_.push(a),
            _ if a.clean.is_none() && !o.rm => need_rm.push(a),
            Safety::Caution if !o.caution => caution.push(a),
            Safety::Caution => add(a, false, &mut batch, &mut one_by_one),
            Safety::Safe => add(a, true, &mut batch, &mut one_by_one),
        }
    }

    let mut freed = 0u64;
    let mut failed = 0;
    let mut run = |j: &Job| {
        if o.dry_run {
            let mut wh = String::new();
            if !j.dir.as_os_str().is_empty() && !j.argv.is_empty() {
                wh = format!("{}  ", dim(&format!("(cd {})", tilde(&j.dir))));
            }
            println!("  would run  {wh}{}{}", j.method(), j.rm_target());
            freed += j.before;
            return;
        }
        if let Err(e) = j.run() {
            eprintln!("  {} {}: {e}", paint("31", "failed"), j.label);
            failed += 1;
        }
        let after = size_of(&j.paths);
        let got = j.before.saturating_sub(after);
        freed += got;
        println!(
            "  {}  {}  freed {}",
            paint("32", "done"),
            j.label,
            human(got)
        );
    };

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
        if o.dry_run || o.yes || ask(&format!("\nRun these {} ({})?", batch.len(), human(total))) {
            batch.iter().for_each(&mut run);
        }
    }
    if !one_by_one.is_empty() {
        println!("\n{}  asked one by one", paint("33", "Caution"));
        for j in &one_by_one {
            if o.dry_run
                || ask(&format!(
                    "{}{}  {} ({})?",
                    j.method(),
                    j.rm_target(),
                    j.label,
                    human(j.before)
                ))
            {
                run(j);
            }
        }
    }

    if !caches.is_empty() {
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
            let q = format!(
                "{}  {} {}?{note}",
                c.name,
                human(c.bytes),
                dim(&format!("via {}", c.method()))
            );
            if o.dry_run || ask(&q) {
                run(&j);
            }
        }
    }

    if !need_rm.is_empty() {
        let total: u64 = need_rm.iter().map(|a| a.freed()).sum();
        println!(
            "\n{}  {} ({}) no tool cleans; add -rm to rm -rf:",
            dim("Left alone"),
            plural(need_rm.len(), "artifact"),
            human(total)
        );
        for a in &need_rm {
            println!("  {}  {}", human(a.freed()), dim(&rel(root, &a.path)));
        }
    }
    if !caution.is_empty() {
        println!(
            "\n{}  {} left alone; add -include-caution to be asked about each",
            paint("33", "Caution"),
            plural(caution.len(), "artifact")
        );
    }
    if !unsafe_.is_empty() {
        println!(
            "\n{}  {} tracked in git, never cleaned",
            paint("31", "Unsafe"),
            plural(unsafe_.len(), "artifact")
        );
    }

    let verb = if o.dry_run { "Would free" } else { "Freed" };
    println!("\n{verb} {}", bold(&human(freed)));
    i32::from(failed > 0)
}

fn size_of(paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .filter(|p| exists(p))
        .map(|p| measure(Path::new(p)).bytes)
        .sum()
}

fn ask(q: &str) -> bool {
    print!("{q} [y/N] ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}
