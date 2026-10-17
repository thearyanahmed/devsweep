use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use crate::caches::Cache;
use crate::rules::Safety;
use crate::scan::Artifact;

fn color() -> bool {
    static COLOR: OnceLock<bool> = OnceLock::new();
    *COLOR.get_or_init(|| {
        std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()) && std::io::stdout().is_terminal()
    })
}

pub fn paint(code: &str, s: &str) -> String {
    if color() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn dim(s: &str) -> String {
    paint("2", s)
}

pub fn bold(s: &str) -> String {
    paint("1", s)
}

pub fn safety_label(s: Safety) -> String {
    let code = match s {
        Safety::Safe => "32",
        Safety::Caution => "33",
        Safety::Unsafe => "31",
    };
    paint(code, s.as_str())
}

pub fn human(b: u64) -> String {
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

pub fn ago(t: Option<SystemTime>) -> String {
    let Some(t) = t else {
        return "-".into();
    };
    let secs = SystemTime::now()
        .duration_since(t)
        .unwrap_or_default()
        .as_secs();
    let (hours, days) = (secs / 3600, secs / 86_400);
    match () {
        _ if hours < 1 => format!("{}m ago", secs / 60),
        _ if hours < 48 => format!("{hours}h ago"),
        _ if days < 60 => format!("{days}d ago"),
        _ => format!("{}mo ago", days / 30),
    }
}

pub fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// Shortens a path under $HOME for display.
pub fn tilde(p: &Path) -> String {
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

pub fn rel(root: &Path, p: &Path) -> String {
    match p.strip_prefix(root) {
        Ok(r) if r.as_os_str().is_empty() => ".".into(),
        Ok(r) => r.display().to_string(),
        Err(_) => tilde(p),
    }
}

pub fn summary(roots: &[PathBuf], arts: &[&Artifact]) {
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

/// Aligns cells into columns two spaces apart, exactly like Go's tabwriter: a cell's width is its character count,
/// color codes included, which gives colored columns their extra breathing room. The last cell of a row isn't padded.
struct Table(Vec<Vec<String>>);

impl Table {
    /// One dim span over the whole header line, split into its cells.
    fn header(cols: &[&str]) -> Self {
        Table(vec![
            dim(&cols.join("\t"))
                .split('\t')
                .map(String::from)
                .collect(),
        ])
    }

    fn row(&mut self, cells: Vec<String>) {
        self.0.push(cells);
    }

    fn print(&self) {
        let cols = self.0.iter().map(Vec::len).max().unwrap_or(0);
        let widths: Vec<usize> = (0..cols)
            .map(|i| {
                self.0
                    .iter()
                    .filter_map(|r| r.get(i))
                    .map(|c| width(c))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        for row in &self.0 {
            let mut line = String::new();
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

fn width(s: &str) -> usize {
    s.chars().count()
}

/// How clean removes an artifact; rm -rf stands out since it needs -rm.
fn method(a: &Artifact) -> String {
    if a.clean.is_none() {
        paint("35", &a.method())
    } else {
        a.method()
    }
}

pub fn print_list(root: &Path, arts: &[&Artifact]) {
    let mut t = Table::header(&[
        "SIZE",
        "SAFETY",
        "KIND",
        "PROJECT",
        "DIR",
        "LAST BUILT",
        "CLEAN WITH",
        "REBUILD WITH",
    ]);
    for a in arts {
        let mut note = a.rebuild.clone();
        if !a.why.is_empty() {
            note += &dim(&format!("  ({})", a.why));
        }
        let mut size = human(a.freed());
        if a.shared > 0 {
            size += &dim(&format!(" +{} shared", human(a.shared)));
        }
        t.row(vec![
            size,
            safety_label(a.safety),
            a.kind.into(),
            rel(root, &a.project),
            a.dir.clone(),
            ago(a.last_built),
            method(a),
            note,
        ]);
    }
    t.print();
}

/// A directory in the tree view; leaves are artifacts.
#[derive(Default)]
struct Node<'a> {
    bytes: u64,
    children: BTreeMap<String, Node<'a>>,
    art: Option<&'a Artifact>,
}

pub fn print_tree(root: &Path, arts: &[&Artifact]) {
    let mut top = Node::default();
    for a in arts {
        let mut n = &mut top;
        n.bytes += a.freed();
        for part in rel(root, &a.path).split('/') {
            n = n.children.entry(part.to_string()).or_default();
            n.bytes += a.freed();
        }
        n.art = Some(a);
    }
    println!("{}  {}", bold(&tilde(root)), human(top.bytes));
    walk_tree(&top, "");
}

fn walk_tree(n: &Node, prefix: &str) {
    let mut kids: Vec<(&String, &Node)> = n.children.iter().collect();
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
        while c.art.is_none() && c.children.len() == 1 {
            let (only_name, only) = c.children.iter().next().unwrap();
            name = format!("{name}/{only_name}");
            c = only;
        }
        if let Some(a) = c.art {
            let mut note = format!("clean: {}, rebuild: {}", method(a), a.rebuild);
            if !a.why.is_empty() {
                note += &format!(" ({})", a.why);
            }
            println!(
                "{prefix}{branch}{}  {}  {}  {}  {}",
                paint("36", &name),
                human(c.bytes),
                safety_label(a.safety),
                dim(&format!("{}, built {}", a.kind, ago(a.last_built))),
                dim(&note),
            );
            continue;
        }
        println!("{prefix}{branch}{name}  {}", dim(&human(c.bytes)));
        walk_tree(c, &format!("{prefix}{next}"));
    }
}

pub fn print_caches(cs: &mut [Cache]) {
    cs.sort_by_key(|c| std::cmp::Reverse(c.bytes));
    let total: u64 = cs.iter().map(|c| c.bytes).sum();
    println!(
        "\n{}  {}  {}",
        bold("Shared caches"),
        human(total),
        dim("(refilled on demand; devsweep clean -caches offers each one)")
    );
    let mut t = Table::header(&["SIZE", "SAFETY", "CACHE", "PATH", "CLEAN WITH"]);
    for c in cs.iter() {
        let path = if c.path.as_os_str().is_empty() {
            "-".into()
        } else {
            tilde(&c.path)
        };
        t.row(vec![
            human(c.bytes),
            safety_label(c.safety),
            c.name.into(),
            path,
            dim(&format!("{}{}", c.method(), notes(c.note))),
        ]);
    }
    t.print();
}

pub fn notes(n: &str) -> String {
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
    fn width_counts_color_codes_like_go() {
        assert_eq!(width("\x1b[32msafe\x1b[0m"), 13);
        assert_eq!(width("└── a"), 5);
    }
}
