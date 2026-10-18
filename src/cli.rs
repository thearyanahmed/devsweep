//! The `devsweep` command: reads the command line, then lists or cleans.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process;
use std::time::{Duration, SystemTime};

use crate::output::{
    Family, Group, Order, ago, bold, dim, group_heading, groups, human, plural, print_by_parent,
    print_list, print_section, print_tree, reset, safety_label, summary, tilde,
};
use crate::scan::Artifact;
use crate::sweep::{Item, Registry, Safety, Section, Sweeper};
use crate::{clean, scan};

/// A command-line flag, Go style: `-name`, `--name`, `-name=value` or `-name value`.
struct Flag {
    name: &'static str,
    /// Takes a value (`-min 1G`); otherwise a switch that is on when given (`-tree`).
    takes_value: bool,
    default: &'static str,
    usage: &'static str,
}

const fn flag(name: &'static str, usage: &'static str) -> Flag {
    Flag {
        name,
        takes_value: false,
        default: "",
        usage,
    }
}

const fn string(name: &'static str, default: &'static str, usage: &'static str) -> Flag {
    Flag {
        name,
        takes_value: true,
        default,
        usage,
    }
}

// In the order -h prints them.
const FLAGS: &[Flag] = &[
    flag(
        "caches",
        "also list shared tool caches (cargo, go, npm, pnpm, brew, ...)",
    ),
    flag(
        "detailed",
        "-tree: also show each artifact's kind, last build, rebuild command and why it isn't safe",
    ),
    flag(
        "dry-run",
        "clean: show what would run, run nothing; rm -rf is always a dry run unless --dry-run=false",
    ),
    string(
        "group",
        "",
        "group the listing by project, kind or safety; image arranges Docker by image",
    ),
    flag(
        "images",
        "only Docker images; with clean, offer each one for removal",
    ),
    flag("json", "print JSON"),
    string(
        "kind",
        "",
        "only these kinds, comma separated: rust,node,go,python,swift,zig,elixir,cmake, or any sweeper added",
    ),
    string(
        "min",
        "1M",
        "hide artifacts smaller than this, e.g. 100M, 1G",
    ),
    flag(
        "models",
        "only local AI models (Ollama, LM Studio); with clean, offer each one for removal",
    ),
    string(
        "older",
        "",
        "only artifacts not built for this long, e.g. 14d, 3w, 2mo, 12h",
    ),
    flag(
        "pwd",
        "the current folder instead of the default ones, as if given as a folder",
    ),
    flag(
        "rm",
        "clean: also rm -rf artifacts and caches no tool has a clean command for (node_modules, .next, ...)",
    ),
    string(
        "sort",
        "size",
        "size, age or path, each its natural way round; add :asc or :desc to choose, e.g. size:asc",
    ),
    flag("tree", "show a tree instead of a table"),
    string(
        "upto",
        "safe",
        "clean: the least safe artifacts to offer: safe, caution (each asked) or unsafe (each typed out; never -y)",
    ),
    flag(
        "volumes",
        "only Docker volumes; with clean, offer each one for removal",
    ),
    flag(
        "y",
        "clean: answer yes to every question but an unsafe one's; what is offered still follows the other flags",
    ),
];

#[expect(
    clippy::struct_excessive_bools,
    reason = "one bool per command-line switch"
)]
#[derive(Debug)]
pub(crate) struct Options {
    pub(crate) tree: bool,
    /// -detailed: the tree shows everything the table does.
    pub(crate) detailed: bool,
    pub(crate) json: bool,
    /// --dry-run as given: `None` leaves tool commands running and rm -rf as a dry run.
    pub(crate) dry_run: Option<bool>,
    /// -upto: the least safe artifacts clean offers.
    pub(crate) upto: Safety,
    pub(crate) rm: bool,
    /// -y: clean answers its own questions.
    pub(crate) yes: bool,
    /// -group image: Docker's containers and volumes under the image they come from.
    pub(crate) by_image: bool,
    pub(crate) older: Option<Duration>,
    pub(crate) min: u64,
    pub(crate) kinds: Option<HashSet<String>>,
    pub(crate) order: Order,
    pub(crate) group: Option<Group>,
    /// Folders were named (or -pwd given): list only what's in them, leaving out what belongs to the whole machine,
    /// such as Docker, unless its flag asks for it.
    pub(crate) scoped: bool,
    /// The string flags as given, for saying which ones hid artifacts.
    raw: BTreeMap<&'static str, String>,
    /// The switches given, for the sections a flag of their own lists: -images, -models.
    switches: HashSet<&'static str>,
}

impl Options {
    /// Whether the flag a section names was given.
    fn flag_on(&self, flag: Option<&str>) -> bool {
        flag.is_some_and(|f| self.switches.contains(f))
    }
}

/// The help text: what you type at full brightness, what explains it faint. A shell that leaves output faint (SGR 2)
/// gets reset first, so the plain parts really are full brightness.
fn help() -> String {
    let flag = spelled;
    let mut h = format!(
        "{}{}{}

{}
  devsweep [flags] [dirs...]         {}
  devsweep clean [flags] [dirs...]   {}
  devsweep why [dirs...]             {}

{}{}{}{}{}{}{}

{}
  {}      {}
  {}   {}
  {}    {}

{}
",
        reset(),
        bold("devsweep"),
        dim(": what your projects' build output is costing you, and cleaning it up."),
        bold("Usage:"),
        dim("list artifacts (default: ~/dev, ~/code, ~/projects, ~/src, ~/Developer)"),
        dim("clean them, after asking"),
        dim("where the disk went: free space, what devsweep can reclaim, Docker's VM disk"),
        dim(
            "Cleaning runs each tool's own command in the project (cargo clean, mix clean, swift package reset, npm run \
             clean,\ngo clean -cache, brew cleanup, docker system prune, ...). Things no tool can clean (node_modules, \
             .next, __pycache__)\nshow as \"rm -rf\" and are only removed with "
        ),
        flag("rm"),
        dim(", and even then only shown until "),
        flag("dry-run"),
        dim("=false.\nWith "),
        flag("caches"),
        dim(", shared tool caches are offered one by one too."),
        bold("Safety:"),
        safety_label(Safety::Safe),
        dim("the project's own tooling recreates it exactly (lockfile present, gitignored)"),
        safety_label(Safety::Caution),
        dim(
            "probably recreatable, but may differ or hold something you want (dist/, no lockfile, not gitignored)"
        ),
        safety_label(Safety::Unsafe),
        dim("git tracks files in it; cleaned only with -upto unsafe, each typed out"),
        bold("Flags:"),
    );
    // Descriptions in a column beside the flags, three spaces past the longest one, as in the sections above.
    let column = FLAGS.iter().map(|f| f.name.len() + 1).max().unwrap_or(0) + 3;
    for f in FLAGS {
        let default = if f.default.is_empty() {
            String::new()
        } else {
            format!(" (default {:?})", f.default)
        };
        let usage = dim(&format!("{}{default}", f.usage));
        writeln!(h, "  {:<column$}{usage}", flag(f.name)).expect("writing to a String can't fail");
    }
    h
}

/// Help asked for goes to stdout and exits 0; help after a mistake goes to stderr and exits 2.
fn usage(mistake: bool) -> ! {
    if mistake {
        eprint!("{}", help());
        process::exit(2)
    }
    print!("{}", help());
    process::exit(0)
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("devsweep: {msg}");
    process::exit(2)
}

/// Why the command line couldn't be used.
#[derive(Debug, PartialEq, Eq)]
enum ArgError {
    /// -h or --help: not a mistake.
    Help,
    /// A flag devsweep doesn't have, as typed.
    Unknown(String),
    /// A flag used wrongly.
    Invalid(String),
}

/// How a flag is written: one dash for a single word (-tree), two for several (--dry-run).
fn spelled(name: &str) -> String {
    if name.contains('-') {
        format!("--{name}")
    } else {
        format!("-{name}")
    }
}

/// A parsed command line.
#[derive(Debug)]
struct Args {
    /// The flags given, by name, with their values ("true" for a switch).
    set: BTreeMap<&'static str, String>,
    dirs: Vec<String>,
}

/// Parses flags anywhere among the dirs (devsweep ~/dev -tree).
fn parse_args(args: &[String]) -> Result<Args, ArgError> {
    let mut set = BTreeMap::new();
    let mut dirs = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--" {
            dirs.extend(it.by_ref().cloned());
            break;
        }
        let (double, body) = arg.strip_prefix("--").map_or_else(
            || (false, arg.strip_prefix('-').unwrap_or_default()),
            |body| (true, body),
        );
        if body.is_empty() {
            dirs.push(arg.clone());
            continue;
        }
        let (name, value) = match body.split_once('=') {
            Some((n, v)) => (n, Some(v.to_owned())),
            None => (body, None),
        };
        if name == "h" || name == "help" {
            return Err(ArgError::Help);
        }
        let Some(f) = FLAGS.iter().find(|f| f.name == name) else {
            return Err(ArgError::Unknown(
                arg.split('=').next().unwrap_or(arg).to_owned(),
            ));
        };
        let flag = spelled(f.name);
        if f.name.contains('-') && !double {
            return Err(ArgError::Invalid(format!("-{name} is written {flag}")));
        }
        let value = match (f.takes_value, value) {
            (false, None) => "true".to_owned(),
            (false, Some(v)) if matches!(v.as_str(), "true" | "false" | "1" | "0") => v,
            (false, Some(v)) => {
                return Err(ArgError::Invalid(format!(
                    "invalid boolean value {v:?} for {flag}"
                )));
            }
            (true, Some(v)) => v,
            (true, None) => it
                .next()
                .cloned()
                .ok_or_else(|| ArgError::Invalid(format!("flag needs an argument: {flag}")))?,
        };
        set.insert(f.name, value);
    }
    Ok(Args { set, dirs })
}

/// A string flag's default value.
fn default_of(name: &str) -> &'static str {
    FLAGS
        .iter()
        .find(|f| f.name == name)
        .map_or("", |f| f.default)
}

/// The options from the flags given, with defaults for the rest.
fn options(set: &BTreeMap<&'static str, String>) -> Options {
    let on = |name| set.get(name).is_some_and(|v| v == "true" || v == "1");
    let get = |name: &str| {
        set.get(name)
            .cloned()
            .unwrap_or_else(|| default_of(name).into())
    };
    let older = get("older");
    let kinds = get("kind");
    Options {
        tree: on("tree"),
        detailed: on("detailed"),
        json: on("json"),
        dry_run: set.get("dry-run").map(|v| v == "true" || v == "1"),
        upto: match get("upto").as_str() {
            "safe" => Safety::Safe,
            "caution" => Safety::Caution,
            "unsafe" => Safety::Unsafe,
            other => fail(format!("bad -upto {other:?}: use safe, caution or unsafe")),
        },
        rm: on("rm"),
        yes: on("y"),
        by_image: get("group") == "image",
        older: (!older.is_empty()).then(|| parse_age(&older).unwrap_or_else(|e| fail(e))),
        min: parse_size(&get("min")).unwrap_or_else(|e| fail(e)),
        kinds: (!kinds.is_empty()).then(|| kinds.split(',').map(|k| k.trim().to_owned()).collect()),
        order: Order::parse(&get("sort")).unwrap_or_else(|e| fail(e)),
        group: match get("group").as_str() {
            "" | "image" => None,
            "project" => Some(Group::Project),
            "kind" => Some(Group::Kind),
            "safety" => Some(Group::Safety),
            other => fail(format!(
                "bad -group {other:?}: use project, kind, safety or image"
            )),
        },
        scoped: false,
        raw: ["older", "kind", "min"]
            .into_iter()
            .map(|n| (n, get(n)))
            .collect(),
        switches: set.keys().copied().filter(|&n| on(n)).collect(),
    }
}

/// Where projects usually live, scanned when no folders are named: whichever of these exist under the home folder.
const DEFAULT_ROOTS: &[&str] = &["dev", "code", "projects", "src", "Developer"];

/// The folders to scan as absolute paths. Named ones must be folders devsweep can read, or it exits saying which and
/// why; with none named, the [`DEFAULT_ROOTS`] that exist, which may be none.
fn resolve_roots(dirs: &[String]) -> Vec<PathBuf> {
    if dirs.is_empty() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        return DEFAULT_ROOTS
            .iter()
            .map(|d| home.join(d))
            .filter(|d| readable(d).is_ok())
            .collect();
    }
    dirs.iter()
        .map(|d| {
            let abs =
                normalize(&std::path::absolute(d).unwrap_or_else(|e| fail(format!("{d}: {e}"))));
            if let Err(why) = readable(&abs) {
                fail(format!("{d}: {why}"));
            }
            abs
        })
        .collect()
}

/// Whether `dir` is a folder devsweep can list, and if not, why in words.
fn readable(dir: &Path) -> Result<(), String> {
    match std::fs::metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err("no such folder".into()),
        Err(e) => return Err(access(&e)),
        Ok(m) if !m.is_dir() => return Err("not a folder".into()),
        Ok(_) => {}
    }
    std::fs::read_dir(dir).map(drop).map_err(|e| access(&e))
}

/// An error opening a folder, in words. macOS's privacy settings refuse with EPERM rather than EACCES: they keep
/// ~/Desktop, ~/Documents, ~/Downloads and others from a terminal until it's given access.
fn access(e: &std::io::Error) -> String {
    const EPERM: i32 = 1;
    if cfg!(target_os = "macos") && e.raw_os_error() == Some(EPERM) {
        return "macOS keeps it from your terminal: allow it in System Settings > Privacy & Security > Files and \
                Folders, or Full Disk Access"
            .into();
    }
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        return "devsweep can't read it (permission denied)".into();
    }
    e.to_string()
}

/// Says why nothing was scanned when no folders were named and none of the usual ones exist.
fn no_roots_note() {
    let tried: Vec<String> = DEFAULT_ROOTS.iter().map(|d| format!("~/{d}")).collect();
    println!(
        "{}",
        dim(&format!(
            "no project folders: none of {} exists; name the folders that hold yours, e.g. devsweep ~/work",
            tried.join(", ")
        ))
    );
}

/// Runs devsweep with the command line it was started with, and exits.
///
/// # Panics
///
/// Only if one of its own worker threads panicked, which is a bug.
pub fn run(registry: &Registry) {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let clean = args.first().is_some_and(|a| a == "clean");
    let why = args.first().is_some_and(|a| a == "why");
    if clean || why {
        args.remove(0);
    }
    let Args { set, mut dirs } = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(ArgError::Help) => usage(false),
        Err(ArgError::Unknown(flag)) => {
            eprintln!("flag provided but not defined: {flag}");
            usage(true)
        }
        Err(ArgError::Invalid(why)) => fail(why),
    };
    let mut o = options(&set);
    if set.contains_key("pwd") {
        dirs.push(".".to_owned());
    }
    o.scoped = !dirs.is_empty();
    // Where the disk went takes in everything devsweep can find, not just what the listing shows by default.
    if why {
        o.switches.insert("caches");
        o.scoped = false;
    }
    let roots = resolve_roots(&dirs);
    if clean && let Err(why) = clean::preflight(&roots) {
        fail(why);
    }

    let sections = registry.sections();
    // A flag for a section listed anyway (-images) lists only it; one for a section that waits to be asked (-caches)
    // adds it to everything else.
    let only = !clean
        && !why
        && !o.json
        && registry
            .listers()
            .filter(|s| s.by_default())
            .any(|s| s.sections().iter().any(|sec| o.flag_on(sec.flag)));
    let (all, items) = gather(registry, &roots, &o, clean, only);
    let shown = filter(&all, &o);

    if why {
        crate::why::report(&roots, &all, &items);
        return;
    }
    if clean {
        if roots.is_empty() {
            no_roots_note();
        }
        let offered: Vec<(Section, Vec<&Item>)> = sections
            .iter()
            .filter(|sec| sec.listed && (sec.flag.is_none() || o.flag_on(sec.flag)))
            .map(|sec| (sec.clone(), of_kind(&items, sec.kind)))
            .filter(|(_, items)| !items.is_empty())
            .collect();
        process::exit(clean::run_clean(&roots, &shown, &offered, &o));
    }
    if o.json {
        print_json(&roots, &shown, &items);
        return;
    }
    if !only {
        print_listing(&roots, &all, &shown, &o);
    }
    print_sections(&sections, &items, &o, only);
}

fn of_kind<'a>(items: &'a [Item], kind: &str) -> Vec<&'a Item> {
    items.iter().filter(|i| i.kind == kind).collect()
}

/// Scans the folders and, at the same time, asks the sweepers that list things on their own; they don't depend on
/// the projects. Returns the artifacts found and the items listed, biggest first.
fn gather(
    registry: &Registry,
    roots: &[PathBuf],
    o: &Options,
    clean: bool,
    only: bool,
) -> (Vec<Artifact>, Vec<Item>) {
    let wanted = |s: &dyn Sweeper| {
        let sections = s.sections();
        let flagged = sections.iter().any(|sec| o.flag_on(sec.flag));
        if only || clean {
            flagged
                || (clean
                    && !o.scoped
                    && sections.iter().any(|sec| sec.listed && sec.flag.is_none()))
        } else {
            flagged || (s.by_default() && !o.scoped)
        }
    };
    std::thread::scope(|scope| {
        #[expect(
            clippy::needless_collect,
            reason = "every thread has to start before the scan does"
        )]
        let listing: Vec<_> = registry
            .listers()
            .filter(|s| wanted(*s))
            .map(|s| scope.spawn(move || measured(s.list())))
            .collect();
        let all = if only {
            Vec::new()
        } else {
            scan(roots, registry)
        };
        let mut items: Vec<Item> = listing
            .into_iter()
            .flat_map(|h| h.join().expect("a sweeper's thread panicked"))
            .collect();
        items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.label.cmp(&b.label)));
        (all, items)
    })
}

/// Every listed section with something in it, each after a blank line; with -images or -volumes, only theirs.
fn print_sections(sections: &[Section], items: &[Item], o: &Options, only: bool) {
    let mut first = only;
    for sec in sections
        .iter()
        .filter(|sec| sec.listed && (!only || o.flag_on(sec.flag)))
    {
        let mine = of_kind(items, sec.kind);
        if mine.is_empty() {
            continue;
        }
        if !std::mem::take(&mut first) {
            println!();
        }
        print_items(sec, &mine, sections, items, o);
    }
    if only && first {
        println!("{}", dim("nothing in Docker (or Docker isn't running)"));
    }
}

/// Items with a folder and no size get measured, all in one pass.
fn measured(mut items: Vec<Item>) -> Vec<Item> {
    let unmeasured: Vec<usize> = (0..items.len())
        .filter(|&i| items[i].bytes.is_none() && items[i].path.is_some())
        .collect();
    let paths: Vec<&Path> = unmeasured
        .iter()
        .filter_map(|&i| items[i].path.as_deref())
        .collect();
    let sizes = scan::measure_all(&paths);
    for (&i, m) in unmeasured.iter().zip(sizes) {
        items[i].bytes = Some(m.bytes);
    }
    items
}

/// One section: a table, or with -group image, each item with what was made from it, then the things those use that
/// nothing does.
fn print_items(sec: &Section, mine: &[&Item], sections: &[Section], items: &[Item], o: &Options) {
    let ids: HashSet<&str> = mine.iter().map(|i| i.id.as_str()).collect();
    let children: Vec<&Item> = items
        .iter()
        .filter(|i| i.parent.as_deref().is_some_and(|p| ids.contains(p)))
        .collect();
    let used: HashSet<&str> = children
        .iter()
        .flat_map(|c| c.uses.iter().map(String::as_str))
        .collect();
    // A section whose items the grouped view already showed lists only the ones nothing uses.
    let grouped_elsewhere = o.by_image && !sec.unused_title.is_empty();
    if grouped_elsewhere {
        let unused: Vec<&Item> = mine
            .iter()
            .copied()
            .filter(|i| i.used_by.is_empty())
            .collect();
        if !unused.is_empty() {
            print_section(sec, &unused, sec.unused_title);
        }
        return;
    }
    if o.by_image && !children.is_empty() {
        let section_of = |kind: &str| sections.iter().find(|s| s.kind == kind);
        let family = Family {
            parents: sec,
            children: children
                .first()
                .and_then(|c| section_of(&c.kind))
                .unwrap_or(sec),
            uses: items
                .iter()
                .find(|i| used.contains(i.id.as_str()))
                .and_then(|i| section_of(&i.kind)),
        };
        print_by_parent(&family, mine, &children, items);
        return;
    }
    print_section(sec, mine, sec.title);
}

fn print_json(roots: &[PathBuf], shown: &[&Artifact], items: &[Item]) {
    #[derive(serde::Serialize)]
    struct Out<'a> {
        artifacts: &'a [&'a Artifact],
        items: &'a [Item],
        roots: &'a [PathBuf],
    }
    let out = Out {
        artifacts: shown,
        items,
        roots,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&out).expect("paths and strings always serialize")
    );
}

/// The summary, then each root's artifacts as a table or a tree, grouped as asked.
fn print_listing(roots: &[PathBuf], all: &[Artifact], shown: &[&Artifact], o: &Options) {
    if roots.is_empty() {
        no_roots_note();
        return;
    }
    // The summary heads the listing: the note and the table or tree follow it directly.
    summary(roots, shown);
    if all.is_empty() {
        println!("{}", dim("nothing found"));
    }
    let hidden = all.len() - shown.len();
    if hidden > 0 {
        println!("{}", dim(&hidden_note(all, shown, hidden, o)));
    }
    for root in roots {
        let under: Vec<&Artifact> = shown
            .iter()
            .copied()
            .filter(|a| a.path.starts_with(root) && a.path != *root)
            .collect();
        if under.is_empty() {
            continue;
        }
        // The summary line names a single root; with several, each gets a heading.
        if roots.len() > 1 {
            let total: u64 = under.iter().map(|a| a.freed()).sum();
            println!("{}  {}", bold(&tilde(root)), human(total));
        }
        match (o.tree, o.group) {
            // The tree already groups by project, folder by folder.
            (true, Some(by @ (Group::Kind | Group::Safety))) => {
                for (name, members) in groups(root, &under, by, o.order) {
                    println!("{}", group_heading(by, &name, &members));
                    print_tree(root, &members, o.detailed);
                }
            }
            (true, _) => print_tree(root, &under, o.detailed),
            (false, group) => print_list(root, &under, group, o.order),
        }
    }
}

fn filter<'a>(all: &'a [Artifact], o: &Options) -> Vec<&'a Artifact> {
    let now = SystemTime::now();
    let mut shown: Vec<&Artifact> = all
        .iter()
        .filter(|a| a.bytes >= o.min)
        .filter(|a| {
            o.older.is_none_or(|older| {
                a.last_built
                    .is_none_or(|t| now.duration_since(t).unwrap_or_default() >= older)
            })
        })
        .filter(|a| o.kinds.as_ref().is_none_or(|k| k.contains(&a.kind)))
        .collect();
    o.order.sort(&mut shown);
    shown
}

/// How many artifacts the filters left out and why, so an empty result isn't a mystery.
fn hidden_note(all: &[Artifact], shown: &[&Artifact], hidden: usize, o: &Options) -> String {
    let left_out = all
        .iter()
        .filter(|a| !shown.iter().any(|s| std::ptr::eq(*s, *a)));
    let bytes: u64 = left_out.clone().map(Artifact::freed).sum();
    let newest = left_out.filter_map(|a| a.last_built).max();
    let mut why: Vec<String> = ["older", "kind", "min"]
        .into_iter()
        .filter(|n| o.raw[n] != default_of(n))
        .map(|n| format!("{} {}", spelled(n), o.raw[n]))
        .collect();
    if why.is_empty() {
        why.push(format!("-min {}", o.raw["min"]));
    }
    format!(
        "{} ({}) hidden by {}; newest built {}",
        plural(hidden, "artifact"),
        human(bytes),
        why.join(", "),
        ago(newest)
    )
}

/// Resolves . and .. without touching the filesystem, like Go's filepath.Clean.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

fn parse_age(s: &str) -> Result<Duration, String> {
    const HOUR: f64 = 3600.0;
    const UNITS: &[(&str, f64)] = &[
        ("mo", 30.0 * 24.0 * HOUR),
        ("y", 365.0 * 24.0 * HOUR),
        ("w", 7.0 * 24.0 * HOUR),
        ("d", 24.0 * HOUR),
        ("h", HOUR),
    ];
    for (suffix, secs) in UNITS {
        if let Some(n) = s.strip_suffix(suffix) {
            if let Ok(v) = n.parse::<f64>()
                && v >= 0.0
            {
                return Ok(Duration::from_secs_f64(v * secs));
            }
            break;
        }
    }
    Err(format!("bad -older {s:?}: use e.g. 12h, 14d, 3w, 2mo, 1y"))
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "the value is checked non-negative, and a fraction of a byte doesn't matter"
)]
fn parse_size(s: &str) -> Result<u64, String> {
    let upper = s.to_uppercase();
    let s = upper.strip_suffix('B').unwrap_or(&upper);
    let s = s.strip_suffix('I').unwrap_or(s);
    let (num, mult) = ["K", "M", "G", "T"]
        .iter()
        .zip(1..)
        .find_map(|(unit, power)| Some((s.strip_suffix(unit)?, 1u64 << (10 * power))))
        .unwrap_or((s, 1));
    match num.parse::<f64>() {
        Ok(v) if v >= 0.0 => Ok((v * mult as f64) as u64),
        _ => Err("bad -min: use e.g. 500K, 100M, 1G".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ages_and_sizes() {
        assert_eq!(parse_age("2w").unwrap(), Duration::from_secs(336 * 3600));
        assert_eq!(parse_age("1mo").unwrap(), Duration::from_secs(720 * 3600));
        assert_eq!(parse_age("12h").unwrap(), Duration::from_secs(12 * 3600));
        assert!(parse_age("5m").is_err());
        assert_eq!(parse_size("100M").unwrap(), 100 << 20);
        assert_eq!(parse_size("1GiB").unwrap(), 1 << 30);
        assert_eq!(parse_size("0").unwrap(), 0);
        assert!(parse_size("lots").is_err());
    }

    #[test]
    fn flags_go_anywhere() {
        let args: Vec<String> = ["~/a", "-tree", "--min=0", "-older", "3d", "~/b"]
            .map(String::from)
            .to_vec();
        let Args { set, dirs } = parse_args(&args).unwrap();
        assert_eq!(dirs, ["~/a", "~/b"]);
        assert_eq!(set["tree"], "true");
        assert_eq!(set["min"], "0");
        assert_eq!(set["older"], "3d");
    }

    #[test]
    fn multi_word_flags_take_two_dashes() {
        let parse =
            |args: &[&str]| parse_args(&args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>());
        assert_eq!(parse(&["--dry-run"]).unwrap().set["dry-run"], "true");
        assert_eq!(parse(&["--dry-run", "-rm"]).unwrap().set.len(), 2);
        assert_eq!(
            parse(&["-dry-run"]).unwrap_err(),
            ArgError::Invalid("-dry-run is written --dry-run".into())
        );
        assert_eq!(parse(&["--tree"]).unwrap().set["tree"], "true");
        assert_eq!(
            parse(&["-nope=1"]).unwrap_err(),
            ArgError::Unknown("-nope".into())
        );
        assert_eq!(parse(&["--help"]).unwrap_err(), ArgError::Help);
    }

    #[test]
    fn named_folders_must_be_readable_folders() {
        let dir = std::env::temp_dir().join(format!("devsweep-readable-{}", process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("file"), "").unwrap();
        assert_eq!(readable(&dir), Ok(()));
        assert_eq!(readable(&dir.join("file")), Err("not a folder".into()));
        assert_eq!(readable(&dir.join("nope")), Err("no such folder".into()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn retired_flags_are_refused() {
        let parse = |arg: &str| parse_args(&[arg.to_owned()]);
        assert_eq!(parse("-yes").unwrap_err(), ArgError::Unknown("-yes".into()));
        assert_eq!(
            parse("--include-caution").unwrap_err(),
            ArgError::Unknown("--include-caution".into())
        );
        assert_eq!(parse("-y").unwrap().set["y"], "true");
        assert_eq!(
            parse("-reverse").unwrap_err(),
            ArgError::Unknown("-reverse".into())
        );
    }
}
