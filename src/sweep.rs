//! The one interface everything devsweep knows about goes through: a [`Sweeper`].
//!
//! A sweeper describes one kind of thing that takes disk space. The engine does the rest: walking folders, measuring,
//! checking git, asking before cleaning, and enforcing the safety net, which no sweeper can switch off.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// What removing something costs you.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Safety {
    /// Recreated exactly by the project's own tooling.
    Safe,
    /// Probably recreatable, but the result may differ or it may hold something by hand.
    Caution,
    /// Removing it loses or changes real content: never cleaned.
    Unsafe,
}

impl Safety {
    /// The word devsweep shows for it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Caution => "caution",
            Self::Unsafe => "unsafe",
        }
    }
}

/// How something is removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clean {
    /// The tool's own command, run in the project: `cargo clean`.
    Tool(Vec<String>),
    /// No tool removes it, so only `rm -rf` of the claimed folder does. devsweep runs that only with `-rm`, and only
    /// as a dry run unless `--dry-run=false`.
    RemoveDir,
    /// It can't be removed now, and why.
    Blocked(String),
}

/// What a sweeper says about a folder it recognises.
#[derive(Clone, Debug)]
pub struct Claim {
    /// How safe removing it is. The engine can only make this stricter: git-tracked files make it unsafe, a folder
    /// git doesn't ignore is at least caution.
    pub safety: Safety,
    /// Why it isn't safe, when it isn't.
    pub why: String,
    /// The command that recreates it.
    pub rebuild: String,
    /// How it's removed.
    pub clean: Clean,
}

/// A container using something, and whether it's running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct User {
    /// The container's name.
    pub name: String,
    /// Whether it's running: then what it uses is unsafe to remove.
    pub running: bool,
}

/// Something a sweeper found on its own, outside any project: a cache, a Docker image or volume.
#[derive(Clone, Debug, Serialize)]
pub struct Item {
    /// Which [`Section`] it's listed under: `cache`, `docker image`.
    pub kind: String,
    /// How other items refer to it, as their [`parent`](Item::parent) or in their [`uses`](Item::uses).
    pub id: String,
    /// Its name as shown.
    pub label: String,
    /// Where it is on disk, when it's a folder. The engine measures it when [`bytes`](Item::bytes) is `None`, and
    /// removes it with rm -rf only when its [`clean`](Item::clean) is [`Clean::RemoveDir`].
    pub path: Option<PathBuf>,
    /// Its size, when the sweeper knows it.
    pub bytes: Option<u64>,
    /// Space it shares with other items, which removing it doesn't free.
    pub shared: u64,
    /// How safe removing it is.
    pub safety: Safety,
    /// Why it's caution or unsafe.
    pub why: String,
    /// A remark shown beside how it's cleaned.
    pub note: String,
    /// How it's removed.
    #[serde(skip)]
    pub clean: Clean,
    /// Containers using it.
    pub used_by: Vec<User>,
    /// What it was made from, by [`id`](Item::id): a container's image.
    pub parent: Option<String>,
    /// What it uses, by [`id`](Item::id): a container's volumes.
    pub uses: Vec<String>,
    /// Whether it's running, for something that runs.
    pub running: Option<bool>,
}

impl Item {
    /// An item of `kind`, named `label`, removed by `clean`; everything else empty.
    #[must_use]
    pub fn new(kind: &str, label: &str, clean: Clean) -> Self {
        Self {
            kind: kind.to_owned(),
            id: label.to_owned(),
            label: label.to_owned(),
            path: None,
            bytes: None,
            shared: 0,
            safety: Safety::Safe,
            why: String::new(),
            note: String::new(),
            clean,
            used_by: Vec::new(),
            parent: None,
            uses: Vec::new(),
            running: None,
        }
    }
}

/// How the items of one kind are listed: a table of their own after the projects.
#[derive(Clone, Debug)]
pub struct Section {
    /// The [`Item::kind`] it lists.
    pub kind: &'static str,
    /// Its heading: `Docker images`.
    pub title: &'static str,
    /// The heading of its name column: `IMAGE`.
    pub noun: &'static str,
    /// A remark after the heading.
    pub note: &'static str,
    /// Shows a PATH column.
    pub paths: bool,
    /// Shows a USED BY column.
    pub users: bool,
    /// Listed at all; containers, say, only show under their image with `-group image`.
    pub listed: bool,
    /// The flag that lists only it, and offers its items to clean: `images`. Without one, it's always listed and
    /// offered.
    pub flag: Option<&'static str>,
    /// Its heading for the items nothing uses, when they're listed apart (with `-group image`).
    pub unused_title: &'static str,
    /// Its heading when its items are listed with what was made from them (`-group image`).
    pub grouped_title: &'static str,
}

/// One kind of thing that takes disk space: a project's build output (`target/`), a tool's cache, Docker's images.
///
/// Folders a sweeper names in [`dirs`](Sweeper::dirs) are offered to [`claim`](Sweeper::claim) as the walk meets
/// them; everything else is up to the engine.
pub trait Sweeper: Send + Sync {
    /// Its name, shown as the kind: `rust`, `node`.
    fn name(&self) -> &str;

    /// The folder names it may claim, so the walk asks it only about those.
    fn dirs(&self) -> &[String] {
        &[]
    }

    /// Whether `dir`, inside `project`, is this sweeper's, and what it is if so. Only called for folders named in
    /// [`dirs`](Sweeper::dirs).
    fn claim(&self, _dir: &Path, _project: &Path) -> Option<Claim> {
        None
    }

    /// The things it finds on its own, outside any project: caches, Docker's images.
    fn list(&self) -> Vec<Item> {
        Vec::new()
    }

    /// How its [`list`](Sweeper::list)ed items are shown, one section per kind.
    fn sections(&self) -> Vec<Section> {
        Vec::new()
    }

    /// Whether it's listed without asking; the shared caches wait for `-caches`.
    fn by_default(&self) -> bool {
        true
    }
}

/// The sweepers devsweep uses, in the order they're asked: the first to claim a folder has it.
#[derive(Default)]
pub struct Registry {
    sweepers: Vec<Box<dyn Sweeper>>,
    /// Folder name to the sweepers that may claim it, in registration order.
    by_dir: HashMap<String, Vec<usize>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.sweepers.iter().map(|s| s.name()).collect();
        f.debug_struct("Registry")
            .field("sweepers", &names)
            .finish_non_exhaustive()
    }
}

impl Registry {
    /// No sweepers at all.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every sweeper devsweep ships with.
    #[must_use]
    pub fn builtin() -> Self {
        let mut r = Self::new();
        crate::builtin::register(&mut r);
        r
    }

    /// Adds a sweeper, asked after the ones already added.
    pub fn add(&mut self, sweeper: impl Sweeper + 'static) -> &mut Self {
        let i = self.sweepers.len();
        for dir in sweeper.dirs() {
            self.by_dir.entry(dir.clone()).or_default().push(i);
        }
        self.sweepers.push(Box::new(sweeper));
        self
    }

    /// The sweepers that list things on their own.
    pub(crate) fn listers(&self) -> impl Iterator<Item = &dyn Sweeper> {
        self.sweepers
            .iter()
            .map(AsRef::as_ref)
            .filter(|s| !s.sections().is_empty())
    }

    /// Every section, in registration order.
    pub(crate) fn sections(&self) -> Vec<Section> {
        self.listers().flat_map(Sweeper::sections).collect()
    }

    /// The first sweeper to claim `dir`, named `name`, inside `project`: its name and its claim.
    pub(crate) fn claim(&self, dir: &Path, project: &Path, name: &str) -> Option<(&str, Claim)> {
        self.by_dir.get(name)?.iter().find_map(|&i| {
            let s = &self.sweepers[i];
            s.claim(dir, project).map(|c| (s.name(), c))
        })
    }
}

/// A sweeper for the common case: folders with given names, recognised by a marker file beside them (or inside them),
/// cleaned and rebuilt by fixed commands.
///
/// ```
/// use devsweep::sweep::SimpleSweeper;
///
/// let gradle = SimpleSweeper::new("gradle")
///     .folders(&["build", ".gradle"])
///     .markers(&["build.gradle", "build.gradle.kts"])
///     .clean(&["./gradlew", "clean"])
///     .rebuild("./gradlew build");
/// ```
#[derive(Clone, Debug)]
pub struct SimpleSweeper {
    name: String,
    dirs: Vec<String>,
    markers: Vec<String>,
    markers_inside: Vec<String>,
    clean: Vec<String>,
    rebuild: String,
    safety: Safety,
}

impl SimpleSweeper {
    /// A sweeper named `name` that claims nothing yet.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            dirs: Vec::new(),
            markers: Vec::new(),
            markers_inside: Vec::new(),
            clean: Vec::new(),
            rebuild: String::new(),
            safety: Safety::Safe,
        }
    }

    /// The folder names it claims.
    #[must_use]
    pub fn folders(mut self, names: &[&str]) -> Self {
        self.dirs = names.iter().map(|d| (*d).to_owned()).collect();
        self
    }

    /// Files beside the folder, any one of which marks it as this kind. With no markers at all, every folder of a
    /// claimed name is.
    #[must_use]
    pub fn markers(mut self, files: &[&str]) -> Self {
        self.markers = files.iter().map(|f| (*f).to_owned()).collect();
        self
    }

    /// Files inside the folder, any one of which marks it as this kind (`CACHEDIR.TAG`).
    #[must_use]
    pub fn markers_inside(mut self, files: &[&str]) -> Self {
        self.markers_inside = files.iter().map(|f| (*f).to_owned()).collect();
        self
    }

    /// The tool's command that removes it, run in the project; `{dir}` stands for the folder's name. Without one, only
    /// `rm -rf` removes it.
    #[must_use]
    pub fn clean(mut self, argv: &[&str]) -> Self {
        self.clean = argv.iter().map(|a| (*a).to_owned()).collect();
        self
    }

    /// The command that recreates it.
    #[must_use]
    pub fn rebuild(mut self, command: &str) -> Self {
        command.clone_into(&mut self.rebuild);
        self
    }

    /// How safe removing it is, when not safe.
    #[must_use]
    pub const fn safety(mut self, safety: Safety) -> Self {
        self.safety = safety;
        self
    }
}

impl Sweeper for SimpleSweeper {
    fn name(&self) -> &str {
        &self.name
    }

    fn dirs(&self) -> &[String] {
        &self.dirs
    }

    fn claim(&self, dir: &Path, project: &Path) -> Option<Claim> {
        let marked = (self.markers.is_empty() && self.markers_inside.is_empty())
            || self.markers.iter().any(|m| exists(&project.join(m)))
            || self.markers_inside.iter().any(|m| exists(&dir.join(m)));
        if !marked {
            return None;
        }
        let name = dir.file_name()?.to_string_lossy();
        let clean = if self.clean.is_empty() {
            Clean::RemoveDir
        } else {
            Clean::Tool(
                self.clean
                    .iter()
                    .map(|a| a.replace("{dir}", &name))
                    .collect(),
            )
        };
        Some(Claim {
            safety: self.safety,
            why: String::new(),
            rebuild: self.rebuild.clone(),
            clean,
        })
    }
}

/// Whether anything is at `p`, symlinks included.
pub(crate) fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}
