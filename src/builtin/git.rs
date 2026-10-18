//! Git's own bloat: `.git` folders grown huge, and worktrees whose repository is gone. Listed so they can be found,
//! never cleaned: what's in them is history, and only you know whether it matters.

use std::fs;
use std::path::{Path, PathBuf};

use crate::output::tilde;
use crate::scan::{measure_all, skipped};
use crate::sweep::{Clean, Item, Safety, Section, Sweeper};

const GIT: &str = "git";

/// A `.git` folder smaller than this isn't worth a mention.
const BIG: u64 = 500 << 20;

/// How deep under a scanned folder to look for repositories.
const DEPTH: usize = 6;

#[derive(Debug)]
pub(super) struct Git;

impl Sweeper for Git {
    fn name(&self) -> &'static str {
        "git"
    }

    fn by_default(&self) -> bool {
        false
    }

    fn sections(&self) -> Vec<Section> {
        vec![Section {
            kind: GIT,
            title: "Git",
            noun: "REPOSITORY",
            note: "(.git folders over 500 MiB, and worktrees whose repository is gone; listed, never cleaned)",
            paths: true,
            users: false,
            listed: true,
            flag: Some("git"),
            unused_title: "",
            grouped_title: "",
        }]
    }

    fn list(&self, roots: &[PathBuf]) -> Vec<Item> {
        let mut repos = Vec::new();
        let mut orphans = Vec::new();
        for root in roots {
            walk(root, 0, &mut repos, &mut orphans);
        }
        let gits: Vec<PathBuf> = repos.iter().map(|r| r.join(".git")).collect();
        let paths: Vec<&Path> = gits
            .iter()
            .map(PathBuf::as_path)
            .chain(orphans.iter().map(PathBuf::as_path))
            .collect();
        let sizes = measure_all(&paths);
        let (git_sizes, orphan_sizes) = sizes.split_at(gits.len());
        let big = gits
            .into_iter()
            .zip(git_sizes)
            .filter(|(_, m)| m.bytes >= BIG)
            .map(|(git, m)| Item {
                bytes: Some(m.bytes),
                safety: Safety::Caution,
                note: "git gc --aggressive --prune=now may shrink it".into(),
                path: Some(git.clone()),
                ..Item::new(
                    GIT,
                    &tilde(git.parent().unwrap_or(&git)),
                    Clean::Blocked("devsweep never touches .git".into()),
                )
            });
        let gone = orphans.into_iter().zip(orphan_sizes).map(|(dir, m)| Item {
            bytes: Some(m.bytes),
            safety: Safety::Caution,
            note: "a worktree whose repository is gone".into(),
            ..Item::new(
                GIT,
                &tilde(&dir),
                Clean::Blocked("delete it yourself if nothing in it matters".into()),
            )
        });
        big.chain(gone).collect()
    }
}

/// Collects the repositories (a `.git` folder) and orphaned worktrees (a `.git` file pointing nowhere) under `dir`,
/// not looking inside either.
fn walk(dir: &Path, depth: usize, repos: &mut Vec<PathBuf>, orphans: &mut Vec<PathBuf>) {
    const SKIP: &[&str] = &["node_modules", "target", "vendor", "Library"];
    let dot_git = dir.join(".git");
    match fs::symlink_metadata(&dot_git) {
        Ok(m) if m.is_dir() => {
            repos.push(dir.to_path_buf());
            return;
        }
        Ok(m) if m.is_file() => {
            if worktree_gone(&dot_git) {
                orphans.push(dir.to_path_buf());
            }
            return;
        }
        _ => {}
    }
    if depth >= DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if e.file_type().is_ok_and(|t| t.is_dir())
            && !name.starts_with('.')
            && !SKIP.contains(&name.as_str())
            && !skipped(dir, &name)
        {
            walk(&e.path(), depth + 1, repos, orphans);
        }
    }
}

/// Whether a worktree's `.git` file (`gitdir: /repo/.git/worktrees/name`) points to a repository that's gone.
fn worktree_gone(dot_git: &Path) -> bool {
    let Ok(text) = fs::read_to_string(dot_git) else {
        return false;
    };
    text.lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        // Relative to the worktree, unless absolute: then it replaces the path whole.
        .is_some_and(|target| !dot_git.with_file_name(target.trim()).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_repos_and_orphaned_worktrees() {
        let root = std::env::temp_dir().join(format!("devsweep-git-{}", std::process::id()));
        fs::create_dir_all(root.join("app/.git")).unwrap();
        fs::create_dir_all(root.join("wt")).unwrap();
        fs::write(root.join("wt/.git"), "gitdir: /nowhere/.git/worktrees/wt\n").unwrap();
        fs::create_dir_all(root.join("live")).unwrap();
        fs::write(
            root.join("live/.git"),
            format!("gitdir: {}\n", root.join("app/.git").display()),
        )
        .unwrap();
        let (mut repos, mut orphans) = (Vec::new(), Vec::new());
        walk(&root, 0, &mut repos, &mut orphans);
        assert_eq!(repos, [root.join("app")]);
        assert_eq!(orphans, [root.join("wt")]);
        fs::remove_dir_all(&root).unwrap();
    }
}
