use std::collections::BTreeMap;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use crate::scan::Artifact;
use crate::sweep::{Clean, Item, Safety, Section, User};

fn color() -> bool {
    static COLOR: OnceLock<bool> = OnceLock::new();
    *COLOR.get_or_init(|| {
        std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()) && std::io::stdout().is_terminal()
    })
}

pub(crate) fn paint(code: &str, s: &str) -> String {
    if color() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_owned()
    }
}

/// Clears whatever styling the terminal was left in, when writing in color.
pub(crate) fn reset() -> &'static str {
    if color() { "\x1b[0m" } else { "" }
}

pub(crate) fn dim(s: &str) -> String {
    paint("2", s)
}

pub(crate) fn bold(s: &str) -> String {
    paint("1", s)
}

pub(crate) fn safety_label(s: Safety) -> String {
    let code = match s {
        Safety::Safe => "32",
        Safety::Caution => "33",
        Safety::Unsafe => "31",
    };
    paint(code, s.as_str())
}

#[expect(clippy::cast_precision_loss, reason = "shown to one decimal place")]
pub(crate) fn human(b: u64) -> String {
    const UNIT: u64 = 1024;
    if b < UNIT {
        return format!("{b} B");
    }
    let (mut div, mut exp) = (UNIT, 0);
    let mut n = b / UNIT;
    while n >= UNIT {
        div *= UNIT;
        exp += 1;
        n /= UNIT;
    }
    format!("{:.1} {}iB", b as f64 / div as f64, b"KMGTPE"[exp] as char)
}

pub(crate) fn ago(t: Option<SystemTime>) -> String {
    let Some(t) = t else {
        return "-".into();
    };
    let secs = SystemTime::now()
        .duration_since(t)
        .unwrap_or_default()
        .as_secs();
    let (hours, days) = (secs / 3600, secs / 86_400);
    match () {
        () if hours < 1 => format!("{}m ago", secs / 60),
        () if hours < 48 => format!("{hours}h ago"),
        () if days < 60 => format!("{days}d ago"),
        () => format!("{}mo ago", days / 30),
    }
}

pub(crate) fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// Shortens a path under $HOME for display.
pub(crate) fn tilde(p: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty())
        && let Ok(rest) = p.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", rest.display())
        };
    }
    p.display().to_string()
}

pub(crate) fn rel(root: &Path, p: &Path) -> String {
    match p.strip_prefix(root) {
        Ok(r) if r.as_os_str().is_empty() => ".".into(),
        Ok(r) => r.display().to_string(),
        Err(_) => tilde(p),
    }
}

pub(crate) fn summary(roots: &[PathBuf], arts: &[&Artifact]) {
    let mut by = [0u64; 3];
    let mut projects: Vec<&Path> = arts.iter().map(|a| a.project.as_path()).collect();
    projects.sort();
    projects.dedup();
    for a in arts {
        by[a.safety as usize] += a.freed();
    }
    let names: Vec<String> = roots.iter().map(|r| tilde(r)).collect();
    println!(
        "{}  {} in {}, {}  {}  {}",
        bold(&names.join(", ")),
        plural(arts.len(), "artifact"),
        plural(projects.len(), "project"),
        paint("32", &format!("{} safe", human(by[0]))),
        paint("33", &format!("{} caution", human(by[1]))),
        paint("31", &format!("{} unsafe", human(by[2]))),
    );
}

/// Aligns cells into columns two spaces apart, like Go's tabwriter, but by the width that shows on screen: color
/// codes don't count, so a dimmed or colored column lines up with its header. The last cell of a row isn't padded.
struct Table {
    rows: Vec<Row>,
    /// Spaces before every row of cells, so they sit under a group's heading.
    indent: usize,
}

#[derive(Debug)]
enum Row {
    Cells(Vec<String>),
    /// A line of its own, such as a group's heading: printed as is, and not part of any column.
    Line(String),
}

impl Table {
    /// One dim span over the whole header line, split into its cells.
    fn new(cols: &[&str], indent: usize) -> Self {
        let header = dim(&cols.join("\t"))
            .split('\t')
            .map(String::from)
            .collect();
        Self {
            rows: vec![Row::Cells(header)],
            indent,
        }
    }

    fn row(&mut self, cells: Vec<String>) {
        self.rows.push(Row::Cells(cells));
    }

    fn line(&mut self, line: String) {
        self.rows.push(Row::Line(line));
    }

    fn print(&self) {
        let cells = || {
            self.rows.iter().filter_map(|r| match r {
                Row::Cells(c) => Some(c),
                Row::Line(_) => None,
            })
        };
        let cols = cells().map(Vec::len).max().unwrap_or(0);
        let widths: Vec<usize> = (0..cols)
            .map(|i| {
                cells()
                    .filter_map(|r| r.get(i))
                    .map(|c| width(c))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        for row in &self.rows {
            let Row::Cells(row) = row else {
                if let Row::Line(line) = row {
                    println!("{line}");
                }
                continue;
            };
            let mut line = " ".repeat(self.indent);
            for (i, cell) in row.iter().enumerate() {
                line += cell;
                if i + 1 < row.len() {
                    line += &" ".repeat(widths[i] - width(cell) + 2);
                }
            }
            println!("{line}");
        }
    }
}

/// Characters that show on screen, skipping color codes (ESC [ ... m).
fn width(s: &str) -> usize {
    let mut n = 0;
    let mut in_code = false;
    for c in s.chars() {
        match (in_code, c) {
            (false, '\x1b') => in_code = true,
            (true, 'm') => in_code = false,
            (true, _) => {}
            (false, _) => n += 1,
        }
    }
    n
}

/// How clean removes an artifact: a tool's command dim like the other details, rm -rf in magenta since it needs -rm.
fn method(a: &Artifact) -> String {
    if a.clean == Clean::RemoveDir {
        paint("35", &a.method())
    } else {
        dim(&a.method())
    }
}

/// What -sort orders by, each with its natural direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortBy {
    /// Descending: the most to free first.
    Size,
    /// Descending: the longest unbuilt first.
    Age,
    /// Ascending: A to Z.
    Path,
}

/// -sort: what to order by, and whether :asc or :desc turned it away from its natural direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Order {
    pub by: SortBy,
    pub reverse: bool,
}

impl Order {
    /// Reads -sort: `size`, `age` or `path`, optionally with `:asc` or `:desc`.
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        let bad =
            || format!("bad -sort {s:?}: use size, age or path, optionally with :asc or :desc");
        let (name, direction) = s.split_once(':').map_or((s, None), |(n, d)| (n, Some(d)));
        // Each order's natural direction: whether it reads descending.
        let (by, descending) = match name {
            "size" => (SortBy::Size, true),
            "age" => (SortBy::Age, true),
            "path" => (SortBy::Path, false),
            _ => return Err(bad()),
        };
        let wants_descending = match direction {
            None => descending,
            Some("desc") => true,
            Some("asc") => false,
            Some(_) => return Err(bad()),
        };
        Ok(Self {
            by,
            reverse: wants_descending != descending,
        })
    }

    /// Sorts artifacts in this order.
    pub(crate) fn sort(self, arts: &mut [&Artifact]) {
        match self.by {
            SortBy::Size => arts.sort_by_key(|a| std::cmp::Reverse(a.freed())),
            SortBy::Age => arts.sort_by_key(|a| a.last_built),
            SortBy::Path => arts.sort_by(|a, b| a.path.cmp(&b.path)),
        }
        if self.reverse {
            arts.reverse();
        }
    }
}

/// What -group puts each artifact under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Group {
    Project,
    Kind,
    Safety,
}

impl Group {
    fn key(self, root: &Path, a: &Artifact) -> String {
        match self {
            Self::Project => rel(root, &a.project),
            Self::Kind => a.kind.clone(),
            Self::Safety => a.safety.as_str().to_owned(),
        }
    }
}

/// The artifacts under each group, keeping their order within it. Groups follow -sort too, direction included: by their total
/// size, their longest unbuilt artifact, or their name. Safety groups keep their own order instead: safe, caution,
/// unsafe.
pub(crate) fn groups<'a>(
    root: &Path,
    arts: &[&'a Artifact],
    by: Group,
    order: Order,
) -> Vec<(String, Vec<&'a Artifact>)> {
    let mut groups: Vec<(String, Vec<&Artifact>)> = Vec::new();
    for &a in arts {
        let key = by.key(root, a);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, members)) => members.push(a),
            None => groups.push((key, vec![a])),
        }
    }
    if by == Group::Safety {
        groups.sort_by_key(|(_, m)| m.first().map(|a| a.safety));
        return groups;
    }
    match order.by {
        SortBy::Path => groups.sort_by(|a, b| a.0.cmp(&b.0)),
        SortBy::Age => groups.sort_by_key(|(_, m)| m.iter().filter_map(|a| a.last_built).min()),
        SortBy::Size => {
            groups
                .sort_by_key(|(_, m)| std::cmp::Reverse(m.iter().map(|a| a.freed()).sum::<u64>()));
        }
    }
    if order.reverse {
        groups.reverse();
    }
    groups
}

/// A group's heading: its name, in its safety's color when grouped by safety, then how much is in it.
pub(crate) fn group_heading(by: Group, name: &str, members: &[&Artifact]) -> String {
    let total: u64 = members.iter().map(|a| a.freed()).sum();
    let title = match members.first() {
        Some(a) if by == Group::Safety => bold(&safety_label(a.safety)),
        _ => bold(name),
    };
    format!(
        "{}  {}",
        title,
        dim(&format!(
            "{}, {}",
            plural(members.len(), "artifact"),
            human(total)
        ))
    )
}

pub(crate) fn print_list(root: &Path, arts: &[&Artifact], group: Option<Group>, order: Order) {
    const COLS: [&str; 8] = [
        "SIZE",
        "SAFETY",
        "KIND",
        "PROJECT",
        "DIR",
        "LAST BUILT",
        "CLEAN WITH",
        "REBUILD WITH",
    ];
    // A group's heading already says its kind or project: leave that column out.
    let left_out = match group {
        Some(Group::Safety) => Some(1),
        Some(Group::Kind) => Some(2),
        Some(Group::Project) => Some(3),
        None => None,
    };
    let keep = |cells: Vec<String>| -> Vec<String> {
        cells
            .into_iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != left_out)
            .map(|(_, c)| c)
            .collect()
    };
    let cols: Vec<&str> = COLS
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != left_out)
        .map(|(_, c)| *c)
        .collect();
    let mut t = Table::new(&cols, if group.is_some() { 2 } else { 0 });
    let rows = |t: &mut Table, arts: &[&Artifact]| {
        for a in arts {
            let mut note = a.rebuild.clone();
            if !a.why.is_empty() {
                note = format!("{note}  ({})", a.why);
            }
            let mut size = human(a.freed());
            if a.shared > 0 {
                size += &dim(&format!(" +{} shared", human(a.shared)));
            }
            t.row(keep(vec![
                size,
                safety_label(a.safety),
                a.kind.clone(),
                rel(root, &a.project),
                a.dir.clone(),
                dim(&ago(a.last_built)),
                method(a),
                dim(&note),
            ]));
        }
    };
    match group {
        None => rows(&mut t, arts),
        Some(by) => {
            for (name, members) in groups(root, arts, by, order) {
                t.line(group_heading(by, &name, &members));
                rows(&mut t, &members);
            }
        }
    }
    t.print();
}

/// A directory in the tree view; leaves are artifacts.
#[derive(Default)]
struct Node<'a> {
    bytes: u64,
    children: BTreeMap<String, Self>,
    art: Option<&'a Artifact>,
}

/// The artifacts as a tree of the folders under `root`, which the lines above have already named.
/// One line per artifact: its size, safety and how it's cleaned; `detailed` adds its kind, last build, how it's
/// rebuilt and why it isn't safe.
pub(crate) fn print_tree(root: &Path, arts: &[&Artifact], detailed: bool) {
    let mut top = Node::default();
    for a in arts {
        let mut n = &mut top;
        n.bytes += a.freed();
        for part in rel(root, &a.path).split('/') {
            n = n.children.entry(part.to_owned()).or_default();
            n.bytes += a.freed();
        }
        n.art = Some(a);
    }
    walk_tree(&top, "", detailed);
}

fn walk_tree(n: &Node<'_>, prefix: &str, detailed: bool) {
    let mut kids: Vec<(&String, &Node<'_>)> = n.children.iter().collect();
    kids.sort_by_key(|k| std::cmp::Reverse(k.1.bytes));
    let last = kids.len().saturating_sub(1);
    for (i, (name, mut c)) in kids.into_iter().enumerate() {
        let (branch, next) = if i == last {
            ("└── ", "    ")
        } else {
            ("├── ", "│   ")
        };
        // Fold single-child chains (a/b/c) into one line, the way most tree views do.
        let mut name = name.clone();
        while c.art.is_none() {
            let mut children = c.children.iter();
            let (Some((only_name, only)), None) = (children.next(), children.next()) else {
                break;
            };
            name = format!("{name}/{only_name}");
            c = only;
        }
        if let Some(a) = c.art {
            let mut line = format!(
                "{prefix}{branch}{}  {}  {}  {}",
                paint("36", &name),
                human(c.bytes),
                safety_label(a.safety),
                method(a),
            );
            if detailed {
                let mut more = format!(
                    "{}, built {}, rebuild: {}",
                    a.kind,
                    ago(a.last_built),
                    a.rebuild
                );
                if !a.why.is_empty() {
                    more = format!("{more} ({})", a.why);
                }
                line = format!("{line}  {}", dim(&more));
            }
            println!("{line}");
            continue;
        }
        println!("{prefix}{branch}{name}  {}", dim(&human(c.bytes)));
        walk_tree(c, &format!("{prefix}{next}"), detailed);
    }
}

/// The containers using something, each with its state.
fn users(used_by: &[User]) -> String {
    if used_by.is_empty() {
        return dim("-");
    }
    used_by
        .iter()
        .map(|c| {
            format!(
                "{} {}",
                c.name,
                dim(if c.running { "running" } else { "stopped" })
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// How an item is removed: the tool's command, rm -rf of its folder, or what's in the way.
pub(crate) fn item_method(i: &Item) -> String {
    match &i.clean {
        Clean::Tool(argv) => argv.join(" "),
        Clean::RemoveDir => format!(
            "rm -rf {}",
            i.path.as_deref().map(tilde).unwrap_or_default()
        ),
        Clean::Blocked(why) => why.clone(),
    }
}

/// An item's own size, and what it shares with others, which removing it doesn't free.
fn item_size(i: &Item) -> String {
    let bytes = human(i.bytes.unwrap_or(0));
    if i.shared > 0 {
        format!("{bytes}{}", dim(&format!(" +{} shared", human(i.shared))))
    } else {
        bytes
    }
}

/// One section's items as a table under its heading.
pub(crate) fn print_section(sec: &Section, items: &[&Item], title: &str) {
    let total: u64 = items.iter().filter_map(|i| i.bytes).sum();
    println!("{}  {}  {}", bold(title), human(total), dim(sec.note));
    let mut cols = vec!["SIZE", "SAFETY", sec.noun];
    if sec.paths {
        cols.push("PATH");
    }
    if sec.users {
        cols.push("USED BY");
    }
    cols.push("CLEAN WITH");
    let mut t = Table::new(&cols, 0);
    for i in items {
        let mut row = vec![item_size(i), safety_label(i.safety), i.label.clone()];
        if sec.paths {
            row.push(i.path.as_deref().map_or_else(|| "-".into(), tilde));
        }
        if sec.users {
            row.push(users(&i.used_by));
        }
        row.push(dim(&format!("{}{}", item_method(i), notes(&i.note))));
        t.row(row);
    }
    t.print();
}

/// What `-group image` shows: a section's items with what was made from them.
#[derive(Debug)]
pub(crate) struct Family<'a> {
    /// The parents' section: Docker images.
    pub parents: &'a Section,
    /// The children's section: Docker containers.
    pub children: &'a Section,
    /// The section of what the children use: Docker volumes.
    pub uses: Option<&'a Section>,
}

/// Each parent, then the children made from it with their state, size and what they use.
pub(crate) fn print_by_parent(f: &Family<'_>, parents: &[&Item], children: &[&Item], all: &[Item]) {
    let size_of = |id: &str| {
        all.iter()
            .find(|i| i.id == id)
            .and_then(|i| i.bytes)
            .unwrap_or(0)
    };
    let uses_noun = f
        .uses
        .map_or_else(|| "USES".to_owned(), |u| format!("{}S", u.noun));
    let mut t = Table::new(&[f.children.noun, "STATE", "SIZE", &uses_noun], 2);
    for p in parents {
        let made: Vec<&&Item> = children
            .iter()
            .filter(|c| c.parent.as_deref() == Some(p.id.as_str()))
            .collect();
        let about = if made.is_empty() {
            p.why.clone()
        } else {
            plural(made.len(), &f.children.noun.to_lowercase())
        };
        t.line(format!(
            "{}  {}  {}  {}",
            bold(&p.id),
            item_size(p),
            safety_label(p.safety),
            dim(&about)
        ));
        for c in made {
            let uses = c
                .uses
                .iter()
                .map(|u| format!("{u} {}", dim(&human(size_of(u)))))
                .collect::<Vec<_>>()
                .join(", ");
            t.row(vec![
                c.label.clone(),
                dim(if c.running == Some(true) {
                    "running"
                } else {
                    "stopped"
                }),
                human(c.bytes.unwrap_or(0)),
                if uses.is_empty() { dim("-") } else { uses },
            ]);
        }
    }
    println!("{}", bold(f.parents.grouped_title));
    t.print();
}

pub(crate) fn notes(n: &str) -> String {
    if n.is_empty() {
        String::new()
    } else {
        format!("  ({n})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanizes_like_go() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1023), "1023 B");
        assert_eq!(human(1024), "1.0 KiB");
        assert_eq!(human(909 * 1024 * 1024 + 100 * 1024), "909.1 MiB");
        assert_eq!(human(3 << 30), "3.0 GiB");
    }

    #[test]
    fn width_skips_color_codes() {
        assert_eq!(width("\x1b[32msafe\x1b[0m"), 4);
        assert_eq!(width("└── a"), 5);
    }

    /// An artifact at /dev/<path>, of `mib` MiB, built `hours` ago.
    fn artifact(path: &str, kind: &'static str, mib: u64, hours: u64, safety: Safety) -> Artifact {
        let path = PathBuf::from("/dev").join(path);
        Artifact {
            project: path.parent().unwrap().to_path_buf(),
            dir: path.file_name().unwrap().to_string_lossy().into_owned(),
            path,
            kind: kind.to_owned(),
            bytes: mib << 20,
            shared: 0,
            files: 1,
            last_built: Some(SystemTime::now() - std::time::Duration::from_secs(hours * 3600)),
            safety,
            why: String::new(),
            rebuild: String::new(),
            clean: Clean::RemoveDir,
        }
    }

    fn order(s: &str) -> Order {
        Order::parse(s).unwrap()
    }

    #[test]
    fn sort_reads_a_direction() {
        let o = |by, reverse| Order { by, reverse };
        assert_eq!(order("size"), o(SortBy::Size, false));
        assert_eq!(order("size:desc"), o(SortBy::Size, false));
        assert_eq!(order("size:asc"), o(SortBy::Size, true));
        assert_eq!(order("age"), o(SortBy::Age, false));
        assert_eq!(order("age:asc"), o(SortBy::Age, true));
        assert_eq!(order("path"), o(SortBy::Path, false));
        assert_eq!(order("path:asc"), o(SortBy::Path, false));
        assert_eq!(order("path:desc"), o(SortBy::Path, true));
        for bad in ["name", "size:up", "size:", ":asc", ""] {
            assert!(Order::parse(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn sort_orders_rows() {
        let all = [
            artifact("b/target", "rust", 30, 5, Safety::Safe),
            artifact("a/node_modules", "node", 10, 1, Safety::Safe),
            artifact("c/target", "rust", 20, 9, Safety::Safe),
        ];
        let names = |s: &str| {
            let mut arts: Vec<&Artifact> = all.iter().collect();
            order(s).sort(&mut arts);
            arts.iter()
                .map(|a| rel(Path::new("/dev"), &a.project))
                .collect::<Vec<_>>()
        };
        assert_eq!(names("size"), ["b", "c", "a"]);
        assert_eq!(names("size:asc"), ["a", "c", "b"]);
        assert_eq!(names("age"), ["c", "b", "a"]);
        assert_eq!(names("age:asc"), ["a", "b", "c"]);
        assert_eq!(names("path"), ["a", "b", "c"]);
        assert_eq!(names("path:desc"), ["c", "b", "a"]);
    }

    #[test]
    fn groups_follow_the_sort_but_safety_keeps_its_order() {
        let all = [
            artifact("a/target", "rust", 30, 1, Safety::Safe),
            artifact("b/target", "rust", 30, 1, Safety::Unsafe),
            artifact("c/node_modules", "node", 50, 1, Safety::Caution),
        ];
        let arts: Vec<&Artifact> = all.iter().collect();
        let keys = |by, s: &str| {
            groups(Path::new("/dev"), &arts, by, order(s))
                .into_iter()
                .map(|(k, _)| k)
                .collect::<Vec<_>>()
        };
        // rust has 60 MiB in all, node 50.
        assert_eq!(keys(Group::Kind, "size"), ["rust", "node"]);
        assert_eq!(keys(Group::Kind, "size:asc"), ["node", "rust"]);
        assert_eq!(keys(Group::Kind, "path"), ["node", "rust"]);
        assert_eq!(keys(Group::Safety, "size"), ["safe", "caution", "unsafe"]);
        assert_eq!(
            keys(Group::Safety, "size:asc"),
            ["safe", "caution", "unsafe"]
        );
    }
}
