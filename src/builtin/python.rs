use std::path::Path;

use crate::sweep::{Claim, Clean, Registry, Safety, SimpleSweeper, Sweeper, exists};

pub(super) fn register(r: &mut Registry) {
    r.add(Venv {
        dirs: vec![".venv".into(), "venv".into()],
    });
    r.add(
        SimpleSweeper::new("python")
            .folders(&[".tox"])
            .markers(&["tox.ini", "pyproject.toml", "setup.cfg"])
            .rebuild("tox"),
    );
    r.add(
        SimpleSweeper::new("python")
            .folders(&[".nox"])
            .markers(&["noxfile.py"])
            .rebuild("nox"),
    );
    r.add(
        SimpleSweeper::new("python")
            .folders(&[".pytest_cache", ".mypy_cache"])
            .rebuild("(cache)"),
    );
    r.add(
        SimpleSweeper::new("python")
            .folders(&[".ruff_cache"])
            .clean(&["ruff", "clean"])
            .rebuild("(cache)"),
    );
    r.add(
        SimpleSweeper::new("python")
            .folders(&["__pycache__"])
            .rebuild("(cache)"),
    );
}

/// The files that pin a virtual environment's packages, and the command that reinstalls them.
const LOCKS: &[(&str, &str)] = &[
    ("uv.lock", "uv sync"),
    ("poetry.lock", "poetry install"),
    ("Pipfile.lock", "pipenv install"),
    ("requirements.txt", "pip install -r requirements.txt"),
];

/// A virtual environment: safe when a lockfile or requirements.txt can recreate it, caution when its packages were
/// installed by hand.
#[derive(Debug)]
struct Venv {
    dirs: Vec<String>,
}

impl Sweeper for Venv {
    fn name(&self) -> &'static str {
        "python"
    }

    fn dirs(&self) -> &[String] {
        &self.dirs
    }

    fn claim(&self, dir: &Path, project: &Path) -> Option<Claim> {
        if !exists(&dir.join("pyvenv.cfg")) {
            return None;
        }
        let lock = LOCKS.iter().find(|(f, _)| exists(&project.join(f)));
        Some(match lock {
            Some((_, cmd)) => Claim {
                safety: Safety::Safe,
                why: String::new(),
                rebuild: (*cmd).into(),
                clean: Clean::RemoveDir,
            },
            None => Claim {
                safety: Safety::Caution,
                why: "no lockfile or requirements.txt: packages were installed by hand".into(),
                rebuild: "recreate by hand".into(),
                clean: Clean::RemoveDir,
            },
        })
    }
}
